//! The deck, drawn in the window: its picture ([`Device`]), the face
//! plates its screen shows ([`plate`]) and its battery ([`battery`]).
//!
//! **The picture is raster.** One PNG for each case colour and appearance,
//! with and without the PTT key lit, drawn from the design's own deck at
//! three times its 196×315 size inside a 256×375 box whose clear margin
//! holds the shadow ([`crate::assets::DEVICE`]). Only the body's edge and
//! the shadow differ between the appearances, which is why the appearance
//! picks the file. [`DeviceGeometry`] says where it all goes at any width:
//! the Deck page shows the deck 164 wide, first run 236.
//!
//! **The plates are drawn**, from `div`s, at any size: a title in the deck's
//! own words and grey schematic marks, one shape for each face, and nothing
//! of what is live on the deck — no names, no card text, no state colours.
//! They are the deck's colours ([`deck_colors`]), the same in both
//! appearances, because they stand for the deck's screen. The design draws
//! seven; Omarchy's is its title alone, and a face this window does not
//! know is the dark screen with nothing on it.
use crate::prefs::Case;
use crate::style::{deck_colors, hex, palette, Palette};
use crate::ui::{ic, icon};
use deck_model::Template;
use gpui::{
    div, img, prelude::*, px, relative, AnyElement, App, Div, FontWeight, Pixels, SharedString,
    Window,
};
use gpui_component::{h_flex, v_flex};

// ---- the battery -------------------------------------------------------------

/// How wide the battery glyph's fill is at `percent`: 17 px is full, and a
/// figure past 100 is full.
pub fn battery_fill(percent: u8) -> f32 {
    17. * f32::from(percent.min(100)) / 100.
}

/// The battery as the sidebar's foot and the Deck page draw it, 26×12: an
/// outline in the sub-line colour, a fill in the text colour as long as the
/// charge, and the cap. The figure beside it is the caller's.
///
/// Three boxes side by side in one that has no border, so their offsets
/// are the design's own: a fill placed inside the outline would be measured
/// from inside the outline's border.
pub fn battery(percent: u8, p: &Palette) -> Div {
    div()
        .relative()
        .flex_none()
        .w(px(26.))
        .h(px(12.))
        // The design's stroke is 1.5 wide on a 21.5×10.5 rectangle at
        // 0.75: its outside is this 23×12 box, its corner 3.2 + 0.75.
        .child(
            div()
                .absolute()
                .left_0()
                .top_0()
                .w(px(23.))
                .h(px(12.))
                .rounded(px(3.95))
                .border(px(1.5))
                .border_color(p.text2),
        )
        .child(
            div()
                .absolute()
                .left(px(3.))
                .top(px(3.))
                .h(px(6.))
                .w(px(battery_fill(percent)))
                .rounded(px(1.4))
                .bg(p.text),
        )
        .child(
            div()
                .absolute()
                .left(px(23.5))
                .top(px(4.))
                .w(px(2.))
                .h(px(4.))
                .rounded(px(1.))
                .bg(p.text2),
        )
}

// ---- the face plates ---------------------------------------------------------

/// The size a plate is drawn at in the design, in which every one of its
/// numbers is given: a plate `size` wide scales them by `size / 124`.
pub const PLATE_DESIGN: f32 = 124.;

/// A plate this size or larger carries its title; smaller, it is the marks
/// alone (the strip's tiles are 79).
pub const PLATE_TITLE_MIN: f32 = 100.;

/// Whether a plate `size` wide shows `title`.
pub fn titled(size: f32, title: &str) -> bool {
    size >= PLATE_TITLE_MIN && !title.is_empty()
}

/// The title a face's plate carries, as the deck writes it, in English:
/// `crate::i18n::kit` has the deck's own words for each of the seven, so a
/// page passes it through `tr`. Omarchy's is a name, and stays as it is.
pub const fn plate_title(face: Template) -> &'static str {
    match face {
        Template::Agents => "AGENTS",
        Template::Macropad => "MACROPAD",
        Template::Usage => "USAGE",
        Template::System => "SYSTEM",
        Template::Timer => "TIMER",
        Template::Arcade => "ARCADE",
        Template::Settings => "SETTINGS",
        Template::Omarchy => "OMARCHY",
    }
}

/// Every face the deck has, in the order its face manager lists them.
pub const FACES: [Template; 8] = [
    Template::Agents,
    Template::Macropad,
    Template::Usage,
    Template::System,
    Template::Timer,
    Template::Arcade,
    Template::Settings,
    Template::Omarchy,
];

/// A face as the deck's own face manager draws it, `size` px square: the
/// deck's dark screen, its title when it is 100 px or more and the caller
/// gave one (in the window's language: see [`plate_title`]), and the face's
/// marks. `radius` is the corner (the strip's tiles use 12; the design's
/// plate is 18 at 124 and scales with it unless said). `None` for `face` is
/// the dark screen alone.
pub fn plate(
    face: Option<Template>,
    size: f32,
    radius: Option<f32>,
    title: Option<SharedString>,
) -> Div {
    let k = size / PLATE_DESIGN;
    let title = title.filter(|title| titled(size, title));
    let has_title = title.is_some();
    v_flex()
        .flex_none()
        .size(px(size))
        .rounded(px(radius.unwrap_or(18. * k)))
        .bg(hex(deck_colors::PLATE_GROUND))
        .py(px(15. * k))
        .px(px(14. * k))
        .gap(px(11. * k))
        .map(|plate| {
            if has_title {
                plate.justify_start()
            } else {
                plate.justify_center()
            }
        })
        .when_some(title, |plate, title| {
            plate.child(
                div()
                    .text_size(px(11. * k))
                    .line_height(px(14. * k))
                    .font_weight(FontWeight::SEMIBOLD)
                    .whitespace_nowrap()
                    .text_color(hex(deck_colors::PLATE_DOT))
                    .child(title),
            )
        })
        .children(face.map(|face| marks(face, k)).unwrap_or_default())
}

/// A face's marks on its plate, every number scaled by `k`.
fn marks(face: Template, k: f32) -> Vec<AnyElement> {
    let u = |v: f32| px(v * k);
    let bar = hex(deck_colors::PLATE_BAR);
    let dot = hex(deck_colors::PLATE_DOT);
    let track = hex(deck_colors::PLATE_TRACK);
    // A bar filled `part` of the way, as the Usage and System faces draw
    // their meters.
    let meter = |height: Pixels, radius: Pixels, part: f32| {
        div()
            .h(height)
            .rounded(radius)
            .bg(track)
            .child(div().h_full().w(relative(part)).rounded(radius).bg(dot))
    };
    match face {
        // Three agents: a dot and a bar each.
        Template::Agents => [64., 46., 56.]
            .into_iter()
            .map(|width| {
                h_flex()
                    .gap(u(8.))
                    .child(div().flex_none().size(u(9.)).rounded_full().bg(dot))
                    .child(
                        div()
                            .flex_none()
                            .h(u(8.))
                            .w(u(width))
                            .rounded(u(4.))
                            .bg(bar),
                    )
                    .into_any_element()
            })
            .collect(),
        // Six keys, three to a row.
        Template::Macropad => vec![div()
            .grid()
            .grid_cols(3)
            .gap(u(7.))
            .children((0..6).map(|_| div().h(u(22.)).rounded(u(5.)).bg(bar)))
            .into_any_element()],
        // One bar for each usage window.
        Template::Usage => [0.60, 0.34, 0.82]
            .into_iter()
            .map(|part| meter(u(9.), u(4.5), part).into_any_element())
            .collect(),
        // Four readings, two to a row: a label and its meter.
        Template::System => vec![div()
            .grid()
            .grid_cols(2)
            .gap(u(12.))
            .children([0.40, 0.70, 0.55, 0.25].map(|part| {
                v_flex()
                    .gap(u(5.))
                    .child(div().h(u(6.)).w(u(16.)).rounded(u(3.)).bg(bar))
                    .child(meter(u(6.), u(3.), part))
            }))
            .into_any_element()],
        // A ring, its upper right half lit. gpui gives a circle one border
        // colour, so the two halves are two masks, one over the other.
        Template::Timer => vec![h_flex()
            .justify_center()
            .child(
                div()
                    .relative()
                    .flex_none()
                    .size(u(54.))
                    .child(
                        icon(ic::PLATE_RING, 54. * k, bar)
                            .absolute()
                            .top_0()
                            .left_0(),
                    )
                    .child(
                        icon(ic::PLATE_RING_HALF, 54. * k, dot)
                            .absolute()
                            .top_0()
                            .left_0(),
                    ),
            )
            .into_any_element()],
        // A cross and two buttons.
        Template::Arcade => vec![h_flex()
            .justify_between()
            .child(
                div()
                    .relative()
                    .flex_none()
                    .size(u(38.))
                    .child(
                        div()
                            .absolute()
                            .left(u(13.))
                            .top_0()
                            .w(u(12.))
                            .h(u(38.))
                            .rounded(u(3.))
                            .bg(bar),
                    )
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top(u(13.))
                            .w(u(38.))
                            .h(u(12.))
                            .rounded(u(3.))
                            .bg(bar),
                    ),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap(u(7.))
                    .child(div().size(u(15.)).rounded_full().bg(dot))
                    .child(div().size(u(15.)).rounded_full().bg(bar)),
            )
            .into_any_element()],
        // Three settings, each a label and a switch: on, off, on.
        Template::Settings => [(48., true), (60., false), (40., true)]
            .into_iter()
            .map(|(width, on)| {
                h_flex()
                    .justify_between()
                    .child(
                        div()
                            .flex_none()
                            .h(u(8.))
                            .w(u(width))
                            .rounded(u(4.))
                            .bg(bar),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .h(u(12.))
                            .w(u(22.))
                            .p(u(2.))
                            .rounded(u(6.))
                            .bg(if on { dot } else { bar })
                            .map(|switch| {
                                if on {
                                    switch.justify_end()
                                } else {
                                    switch.justify_start()
                                }
                            })
                            .child(
                                div()
                                    .h_full()
                                    .w(u(8.))
                                    .rounded_full()
                                    .bg(hex(deck_colors::PLATE_GROUND)),
                            ),
                    )
                    .into_any_element()
            })
            .collect(),
        // Not designed: its title alone.
        Template::Omarchy => Vec::new(),
    }
}

// ---- the device --------------------------------------------------------------

/// The design's deck, in its own units: 196 wide and 315 high.
pub const DECK_SIZE: (f32, f32) = (196., 315.);
/// The clear margin around it in each picture, which holds the shadow.
pub const PICTURE_MARGIN: f32 = 30.;
/// The screen's plate inside the deck: its corner, and its size.
pub const SCREEN_AT: (f32, f32) = (36., 34.);
pub const SCREEN_SIZE: f32 = 124.;
/// A deck that is not linked is its picture at this, with nothing on its
/// screen.
pub const DIM: f32 = 0.45;

/// The splash first run puts on the deck's screen, by asset path.
pub const SPLASH: &str = "device/deck-splash.png";

/// Where the deck's picture and its screen sit, for a deck `width` px wide.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeviceGeometry {
    /// Logical px per design unit.
    pub scale: f32,
    /// The deck's own height: its box is `width` × this.
    pub height: f32,
    /// How far the picture reaches past the box on every side: its shadow.
    pub margin: f32,
    /// The picture's size, margin included.
    pub picture: (f32, f32),
    /// The plate's top-left corner inside the box.
    pub screen: (f32, f32),
    /// The plate's size, a whole number of px as the design rounds it.
    pub plate: f32,
    /// The screen's corner, for the splash (a plate scales its own).
    pub corner: f32,
}

impl DeviceGeometry {
    pub fn at(width: f32) -> DeviceGeometry {
        let s = width / DECK_SIZE.0;
        DeviceGeometry {
            scale: s,
            height: DECK_SIZE.1 * s,
            margin: PICTURE_MARGIN * s,
            picture: (
                (DECK_SIZE.0 + 2. * PICTURE_MARGIN) * s,
                (DECK_SIZE.1 + 2. * PICTURE_MARGIN) * s,
            ),
            screen: (SCREEN_AT.0 * s, SCREEN_AT.1 * s),
            plate: (SCREEN_SIZE * s).round(),
            corner: 18. * s,
        }
    }
}

/// The picture of the deck in `case`, for the dark or the light
/// appearance, with its PTT key lit or not, by asset path.
pub const fn device_picture(case: Case, dark: bool, ptt_lit: bool) -> &'static str {
    match (case, dark, ptt_lit) {
        (Case::White, true, false) => "device/deck-white-dark.png",
        (Case::White, true, true) => "device/deck-white-dark-ptt.png",
        (Case::White, false, false) => "device/deck-white-light.png",
        (Case::White, false, true) => "device/deck-white-light-ptt.png",
        (Case::Black, true, false) => "device/deck-black-dark.png",
        (Case::Black, true, true) => "device/deck-black-dark-ptt.png",
        (Case::Black, false, false) => "device/deck-black-light.png",
        (Case::Black, false, true) => "device/deck-black-light-ptt.png",
    }
}

/// The deck as the owner holds it, `width` px wide: the picture of its case
/// for the window's appearance, and on its screen the plate of the face it
/// shows, the splash, or the dark screen.
///
/// Its box is the deck's own size; the picture's shadow reaches past it on
/// every side (by 30 units), so give it that much room. The picture loads
/// a frame or two after it is first drawn.
#[derive(IntoElement)]
pub struct Device {
    case: Case,
    face: Option<Template>,
    width: f32,
    title: Option<SharedString>,
    dim: bool,
    ptt_lit: bool,
    splash: bool,
}

impl Device {
    /// The deck in `case`, showing `face` (`None`: the dark screen),
    /// `width` px wide.
    pub fn new(case: Case, face: Option<Template>, width: f32) -> Device {
        Device {
            case,
            face,
            width,
            title: None,
            dim: false,
            ptt_lit: false,
            splash: false,
        }
    }

    /// The plate's title in the window's language (`tr(plate_title(face))`);
    /// drawn when the plate is 100 px or more, so on a deck about 158 wide
    /// or wider (the Deck page's 164 is; first run's 236 is).
    pub fn title(mut self, title: impl Into<SharedString>) -> Device {
        self.title = Some(title.into());
        self
    }

    /// Not linked: the picture at 45 %, and nothing on its screen.
    pub fn dim(mut self, dim: bool) -> Device {
        self.dim = dim;
        self
    }

    /// The PTT key lit green, as first run asks the owner to hold it.
    pub fn ptt_lit(mut self, lit: bool) -> Device {
        self.ptt_lit = lit;
        self
    }

    /// The splash on its screen in place of a plate, as first run shows it.
    pub fn splash(mut self, splash: bool) -> Device {
        self.splash = splash;
        self
    }
}

impl RenderOnce for Device {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let dark = palette(cx).dark;
        let g = DeviceGeometry::at(self.width);
        let (left, top) = g.screen;
        // Dimmed, the screen stays the picture's own dark glass: a plate
        // over it would darken it twice, gpui's opacity being per part.
        let screen: Option<AnyElement> = if self.dim {
            None
        } else if self.splash {
            Some(
                img(SPLASH)
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .size(px(g.plate))
                    .rounded(px(g.corner))
                    .into_any_element(),
            )
        } else {
            self.face.map(|face| {
                div()
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .child(plate(Some(face), g.plate, None, self.title))
                    .into_any_element()
            })
        };
        div()
            .relative()
            .flex_none()
            .w(px(self.width))
            .h(px(g.height))
            .child(
                img(device_picture(self.case, dark, self.ptt_lit))
                    .absolute()
                    .left(px(-g.margin))
                    .top(px(-g.margin))
                    .w(px(g.picture.0))
                    .h(px(g.picture.1))
                    .when(self.dim, |picture| picture.opacity(DIM)),
            )
            .children(screen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::Assets;

    #[test]
    fn the_battery_fills_as_far_as_its_charge_and_no_further() {
        assert_eq!(battery_fill(0), 0.);
        assert_eq!(battery_fill(100), 17.);
        assert!((battery_fill(85) - 14.45).abs() < 1e-4);
        assert_eq!(battery_fill(50), 8.5);
        // The deck sends a charge; a figure past full is full.
        assert_eq!(battery_fill(140), 17.);
        assert_eq!(battery_fill(u8::MAX), 17.);
    }

    #[test]
    fn a_plate_carries_its_title_only_from_100_px_up() {
        // The strip's tiles have none; the Deck page's plate (104) and the
        // Usage panel's (116) do.
        assert!(!titled(79., "AGENTS"));
        assert!(!titled(99.9, "AGENTS"));
        assert!(titled(100., "AGENTS"));
        assert!(titled(104., "USAGE"));
        assert!(titled(149., "MACROPAD"));
        // No title, nothing to carry.
        assert!(!titled(116., ""));
        // The Deck page's deck is 164 wide, which is where its plate starts
        // to carry the title; first run's is 236.
        assert_eq!(DeviceGeometry::at(164.).plate, 104.);
        assert!(titled(DeviceGeometry::at(164.).plate, "AGENTS"));
        assert!(!titled(DeviceGeometry::at(150.).plate, "AGENTS"));
    }

    #[test]
    fn every_face_has_a_plate_title_and_the_designed_ones_are_the_decks_own_words() {
        crate::i18n::install();
        let titles: Vec<&str> = FACES.iter().map(|face| plate_title(*face)).collect();
        assert_eq!(
            titles,
            ["AGENTS", "MACROPAD", "USAGE", "SYSTEM", "TIMER", "ARCADE", "SETTINGS", "OMARCHY"]
        );
        for face in FACES {
            let title = plate_title(face);
            // A name is never translated; the other seven have the deck's
            // own words in the kit's table.
            assert_eq!(
                deck_gui_core::i18n::has_entry(title),
                face != Template::Omarchy,
                "{title}"
            );
        }
        // Every face the protocol has is in the list, once.
        for (i, a) in FACES.iter().enumerate() {
            assert!(!FACES[i + 1..].contains(a), "{a:?} twice");
        }
    }

    #[test]
    fn every_picture_is_one_this_app_carries_and_each_is_its_own() {
        let mut seen = Vec::new();
        for case in Case::ALL {
            for dark in [true, false] {
                for lit in [false, true] {
                    let path = device_picture(case, dark, lit);
                    assert!(Assets::picture(path).is_some(), "{path} is not embedded");
                    assert!(path.contains(case.word()), "{path}");
                    assert!(
                        path.contains(if dark { "-dark" } else { "-light" }),
                        "{path}"
                    );
                    assert_eq!(path.ends_with("-ptt.png"), lit, "{path}");
                    assert!(!seen.contains(&path), "{path} twice");
                    seen.push(path);
                }
            }
        }
        assert_eq!(seen.len(), 8);
        assert!(Assets::picture(SPLASH).is_some());
    }

    #[test]
    fn the_deck_scales_from_its_own_196_units_with_the_shadows_margin_around_it() {
        // The design's own size: one unit is one px.
        let full = DeviceGeometry::at(196.);
        assert_eq!(full.scale, 1.);
        assert_eq!(full.height, 315.);
        assert_eq!(full.picture, (256., 375.));
        assert_eq!(
            (full.margin, full.screen, full.plate),
            (30., (36., 34.), 124.)
        );
        // The Deck page: a 104 px plate at about (30.1, 28.4).
        let deck = DeviceGeometry::at(164.);
        assert!((deck.height - 263.57).abs() < 0.01, "{deck:?}");
        assert!((deck.screen.0 - 30.12).abs() < 0.01 && (deck.screen.1 - 28.45).abs() < 0.01);
        assert_eq!(deck.plate, 104.);
        // First run: 149.
        let first = DeviceGeometry::at(236.);
        assert_eq!(first.plate, 149.);
        assert!((first.corner - 21.67).abs() < 0.01);
        // The picture keeps the PNGs' own shape, 768×1125.
        for width in [140., 164., 236.] {
            let g = DeviceGeometry::at(width);
            assert!((g.picture.0 / g.picture.1 - 768. / 1125.).abs() < 1e-4);
            assert!((g.picture.0 - (width + 2. * g.margin)).abs() < 1e-3);
        }
        // Not linked is the design's 45 %.
        assert_eq!(DIM, 0.45);
    }
}
