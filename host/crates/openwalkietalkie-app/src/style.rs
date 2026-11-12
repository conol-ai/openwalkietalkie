//! The window's look, taken from the deck's own screen: an opaque window,
//! greys for surfaces and lines, three text steps, Geist at 11 to 30 px, and
//! colour only where something has a state.
//!
//! The values are the design's tokens (`DARK` and `LIGHT` of the companion
//! app's design), one field each, so a part drawn from the design reads
//! `t['text2']` as `p.text2`. Three rules carry the look and are worth
//! keeping when a value changes:
//!
//! * **Colour means state.** Nothing else in the window is coloured: the
//!   primary button, a switch that is on and a selected item are the neutral
//!   `solid` and `outline`, and keyboard focus is the one blue, `info`.
//! * **A healthy state is a dot and a word; only trouble gets a tinted
//!   pill** (`warn_wash`, `bad_wash`, `info_wash`). There is no good wash.
//! * **Light is designed, not inverted.** Its state colours step down so
//!   they keep their contrast on white, and a word on a wash takes the
//!   darker `*_text` step. The amber wash and rim stay on the bright amber
//!   in both appearances.
//!
//! Everything a screen paints comes from [`Palette`]. gpui-component's own
//! furniture (text fields, tooltips, notifications, menus) reads its `Theme`
//! instead, so [`install`] writes the same values there: the two cannot
//! drift, and a later `Theme::change` cannot quietly undo them because
//! [`install`] is what runs on every appearance change.
use deck_gui_core::i18n::Language;
use deck_model::Tone;
use gpui::{
    px, rgb, rgba, App, Font, FontFallbacks, FontFeatures, FontStyle, FontWeight, Global, Hsla,
    WindowAppearance,
};
use gpui_component::{Theme, ThemeMode};
use std::borrow::Cow;

/// The sans family every screen is set in.
pub const SANS: &str = "Geist";
/// The family for versions, paths, key names and values: anything that is
/// read character by character.
pub const MONO: &str = "Geist Mono";

/// Pins the appearance instead of following the system:
/// `OPENWALKIETALKIE_APPEARANCE=light|dark`. For screenshots and for checking
/// one appearance without changing the Mac's.
pub const APPEARANCE_ENV: &str = "OPENWALKIETALKIE_APPEARANCE";

/// What Chinese and Japanese are drawn in. Geist has no CJK, so without a
/// list CoreText would pick the fallback from the Mac's language order and
/// not from the window's language.
pub const CJK: [&str; 3] = ["PingFang SC", "PingFang TC", "Hiragino Sans"];

/// Things drawn the way the deck draws them. They are the deck's colours,
/// not the window's, so they are the same in both appearances.
pub mod deck_colors {
    /// A face plate's ground: the deck's screen.
    pub const PLATE_GROUND: u32 = 0x050506;
    /// A plate's bars and cells.
    pub const PLATE_BAR: u32 = 0x5D6068;
    /// A plate's dots, its title and a meter's fill.
    pub const PLATE_DOT: u32 = 0x8E9299;
    /// A plate's empty meter track.
    pub const PLATE_TRACK: u32 = 0x33363D;
    /// A macro key's name, centred on the key.
    pub const KEY_NAME: u32 = 0xF2F2F3;
    /// The deck button in a macro key's top-left corner.
    pub const KEY_CORNER: u32 = 0x8E9299;
    /// The shortcut a macro key types, in its top-right corner.
    pub const KEY_COMBO: u32 = 0xA6A9AF;
}

/// Every colour a screen paints, for one appearance. The names are the
/// design's tokens.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    // ---- surfaces
    /// The window, and the main column.
    pub win: Hsla,
    /// The sidebar.
    pub side: Hsla,
    /// Cards, menus and dialogs.
    pub surf: Hsla,
    /// What a control sits in: a field, a popup, a segmented control, a
    /// quiet button, a tile.
    pub well: Hsla,
    /// Behind the deck's picture and a face plate's preview.
    pub stage: Hsla,
    // ---- lines
    /// The rule between rows and around a card.
    pub line: Hsla,
    /// A control's edge: at 3:1 or better against the window and a card,
    /// so a control's outline reads without colour.
    pub edge: Hsla,
    /// What is selected: a segment, a radio, a face tile, the page in the
    /// sidebar. Never a fill.
    pub outline: Hsla,
    // ---- text
    pub text: Hsla,
    /// Sub-lines, values, and icons at rest.
    pub text2: Hsla,
    /// Section labels and anything quieter than a sub-line.
    pub text3: Hsla,
    // ---- the neutral primary
    /// The primary button, a switch that is on, a radio's dot.
    pub solid: Hsla,
    pub on_solid: Hsla,
    // ---- state: the dot, a word on its wash, the wash, the rim
    pub good: Hsla,
    pub warn: Hsla,
    pub bad: Hsla,
    /// Keyboard focus, and "working".
    pub info: Hsla,
    pub good_text: Hsla,
    pub warn_text: Hsla,
    pub bad_text: Hsla,
    pub info_text: Hsla,
    pub warn_wash: Hsla,
    /// The rim of something that needs attention.
    pub warn_line: Hsla,
    pub bad_wash: Hsla,
    pub info_wash: Hsla,
    // ---- deck-shaped things, the same in both appearances
    /// A macro key's face.
    pub key_fill: Hsla,
    pub key_line: Hsla,
    /// The 1 px ring around a face tile at rest.
    pub tile_rest: Hsla,
    /// Not a token of the design: what a dialog dims the window with.
    pub scrim: Hsla,
}

impl Global for Palette {}

/// `0xRRGGBB` as a colour.
pub fn hex(value: u32) -> Hsla {
    rgb(value).into()
}

/// `0xRRGGBBAA` as a colour.
fn hexa(value: u32) -> Hsla {
    rgba(value).into()
}

impl Palette {
    pub fn dark() -> Palette {
        Palette {
            dark: true,
            win: hex(0x0E0E10),
            side: hex(0x131316),
            surf: hex(0x17171A),
            well: hex(0x202024),
            stage: hex(0x101013),
            line: hex(0xFFFFFF).alpha(0.08),
            edge: hex(0x6A6D73),
            outline: hex(0xE8E9EA),
            text: hex(0xF2F2F3),
            text2: hex(0xA6A9AF),
            text3: hex(0x8E9299),
            solid: hex(0xEDEDEF),
            on_solid: hex(0x0E0E10),
            good: hex(0x3DDC84),
            warn: hex(0xFFB020),
            bad: hex(0xFF5C5C),
            info: hex(0x4C9AFF),
            good_text: hex(0x3DDC84),
            warn_text: hex(0xFFB020),
            bad_text: hex(0xFF5C5C),
            info_text: hex(0x4C9AFF),
            warn_wash: hex(0xFFB020).alpha(0.11),
            warn_line: hex(0xFFB020).alpha(0.34),
            bad_wash: hex(0xFF5C5C).alpha(0.12),
            info_wash: hex(0x4C9AFF).alpha(0.13),
            key_fill: hex(0x0B0B0D),
            key_line: hex(0x2C2E33),
            tile_rest: hex(0x3A3D44),
            scrim: hexa(0x00000059),
        }
    }

    pub fn light() -> Palette {
        Palette {
            dark: false,
            win: hex(0xF4F4F6),
            side: hex(0xEBEBEE),
            surf: hex(0xFFFFFF),
            well: hex(0xEEEEF1),
            stage: hex(0xE3E3E7),
            line: hex(0x000000).alpha(0.10),
            edge: hex(0x898C91),
            outline: hex(0x18181A),
            text: hex(0x18181A),
            text2: hex(0x56595F),
            text3: hex(0x63666C),
            solid: hex(0x18181A),
            on_solid: hex(0xFFFFFF),
            good: hex(0x177A42),
            warn: hex(0x9A5B00),
            bad: hex(0xC93535),
            info: hex(0x1D6BD6),
            good_text: hex(0x136B39),
            warn_text: hex(0x8A5100),
            bad_text: hex(0xB42B2B),
            info_text: hex(0x1A5FC0),
            // The amber wash and rim are the bright amber's, not the dark
            // `warn` above: on white a wash of that would read as mud.
            warn_wash: hex(0xFFB020).alpha(0.18),
            warn_line: hex(0xFFB020).alpha(0.60),
            bad_wash: hex(0xC93535).alpha(0.09),
            info_wash: hex(0x1D6BD6).alpha(0.09),
            key_fill: hex(0x0B0B0D),
            key_line: hex(0x2C2E33),
            tile_rest: hex(0x000000).alpha(0.25),
            scrim: hexa(0x00000030),
        }
    }

    /// The palette for `appearance`.
    pub fn for_appearance(appearance: WindowAppearance) -> Palette {
        if is_dark(appearance) {
            Palette::dark()
        } else {
            Palette::light()
        }
    }

    /// A status dot in the firmware's `tone` (§8.2: a red dot here and a red
    /// dot on the deck mean the same thing). Gray is the neutral dot, which
    /// is a control's edge.
    pub fn tone(&self, tone: Tone) -> Hsla {
        match tone {
            Tone::Blue => self.info,
            Tone::Amber => self.warn,
            Tone::Green => self.good,
            Tone::Red => self.bad,
            Tone::Gray => self.edge,
        }
    }

    /// A word in `tone`'s own colour, dark enough for its wash. Gray is the
    /// sub-line colour. (A healthy or neutral state's word is not coloured
    /// at all: see `ui::status`.)
    pub fn tone_text(&self, tone: Tone) -> Hsla {
        match tone {
            Tone::Blue => self.info_text,
            Tone::Amber => self.warn_text,
            Tone::Green => self.good_text,
            Tone::Red => self.bad_text,
            Tone::Gray => self.text2,
        }
    }

    /// The pill a word in `tone` sits on. `None` for a healthy or neutral
    /// state, which is a dot and a word and never a pill.
    pub fn tone_wash(&self, tone: Tone) -> Option<Hsla> {
        match tone {
            Tone::Blue => Some(self.info_wash),
            Tone::Amber => Some(self.warn_wash),
            Tone::Red => Some(self.bad_wash),
            Tone::Green | Tone::Gray => None,
        }
    }
}

/// The palette the window is drawn with.
pub fn palette(cx: &App) -> &Palette {
    cx.global::<Palette>()
}

pub fn is_dark(appearance: WindowAppearance) -> bool {
    matches!(
        appearance,
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

/// What [`APPEARANCE_ENV`] asks for, if anything.
pub fn pinned_appearance() -> Option<bool> {
    match std::env::var(APPEARANCE_ENV).ok()?.trim() {
        s if s.eq_ignore_ascii_case("dark") => Some(true),
        s if s.eq_ignore_ascii_case("light") => Some(false),
        _ => None,
    }
}

/// The CJK families in the order `language` wants them: its own first, so a
/// character the three share (most of them) is drawn the way that language
/// draws it.
pub fn cjk_fallbacks(language: Language) -> Vec<String> {
    let first = match language {
        Language::Japanese => "Hiragino Sans",
        Language::TraditionalChinese => "PingFang TC",
        Language::English | Language::SimplifiedChinese => "PingFang SC",
    };
    std::iter::once(first)
        .chain(CJK.into_iter().filter(|family| *family != first))
        .map(str::to_string)
        .collect()
}

/// The window's font: Geist, then the CJK families in `language`'s order.
///
/// Set once, at the shell's root, because `Styled::font` also sets the
/// weight and the style: a child changes only what it names, and a child's
/// `font_family(MONO)` keeps the fallbacks. All three CJK families are in
/// the list whatever the language, because the language popup names all
/// three languages at once. Tooltips and notifications are views of their
/// own and take the theme's family alone.
pub fn ui_font(language: Language) -> Font {
    Font {
        family: SANS.into(),
        features: FontFeatures::default(),
        fallbacks: Some(FontFallbacks::from_fonts(cjk_fallbacks(language))),
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    }
}

/// Load Geist and Geist Mono from the binary. They are registered with the
/// text system and never served through the `AssetSource`, so a dev build
/// cannot lose them and the asset bundle stays icons and pictures.
pub fn register_fonts(cx: &mut App) {
    let faces: Vec<Cow<'static, [u8]>> = vec![
        Cow::Borrowed(include_bytes!("../assets/fonts/Geist.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../assets/fonts/Geist-Medium.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../assets/fonts/Geist-SemiBold.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../assets/fonts/Geist-Bold.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../assets/fonts/GeistMono.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../assets/fonts/GeistMono-Medium.ttf").as_slice()),
    ];
    if let Err(e) = cx.text_system().add_fonts(faces) {
        // The system font still draws every string; this is a look, not a
        // function, so it is a line on stderr and not a refusal to start.
        eprintln!("openwalkietalkie-app: the bundled fonts did not load ({e})");
    }
}

/// Make `palette` the window's: the global every screen reads, and the
/// gpui-component `Theme` its own widgets read. Runs at boot and again on
/// every appearance change, after `Theme::change` has reset the stock
/// values for the new mode.
pub fn install(palette: Palette, cx: &mut App) {
    let mode = if palette.dark {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    };
    Theme::change(mode, None, cx);
    let p = palette;
    let clear = gpui::transparent_black();
    let theme = Theme::global_mut(cx);
    theme.font_family = SANS.into();
    theme.mono_font_family = MONO.into();
    // One rem stays 16 px (`Root` sets the rem from this), and every
    // screen sets its own sizes in pixels on top of it.
    theme.font_size = px(16.);
    theme.mono_font_size = px(12.);
    theme.radius = px(8.);
    theme.radius_lg = px(12.);
    // The window is flat: a card is a fill and a rule, never a shadow.
    theme.shadow = false;
    let c = &mut theme.colors;
    // `Root` paints this behind the shell, so it is the window's colour.
    c.background = p.win;
    c.foreground = p.text;
    c.muted = p.well;
    c.muted_foreground = p.text2;
    // Menus, tooltips and notifications are framed like the design's menu.
    c.border = p.edge;
    // A text field: its edge, and focus in the one blue.
    c.input = p.edge;
    c.ring = p.info;
    c.caret = p.text;
    c.selection = p.info.alpha(0.30);
    // The row under the pointer or the arrow keys in a menu.
    c.accent = p.well;
    c.accent_foreground = p.text;
    c.popover = p.surf;
    c.popover_foreground = p.text;
    c.overlay = p.scrim;
    c.list = clear;
    c.list_hover = p.well;
    c.list_active = p.well;
    c.list_active_border = clear;
    c.primary = p.solid;
    c.primary_hover = p.solid.opacity(0.9);
    c.primary_active = p.solid.opacity(0.85);
    c.primary_foreground = p.on_solid;
    c.secondary = p.well;
    c.secondary_hover = p.well;
    c.secondary_active = p.well;
    c.secondary_foreground = p.text;
    c.danger = p.bad;
    c.danger_hover = p.bad.opacity(0.9);
    c.danger_active = p.bad.opacity(0.85);
    c.danger_foreground = gpui::white();
    c.warning = p.warn;
    c.warning_foreground = p.on_solid;
    c.success = p.good;
    c.success_foreground = p.on_solid;
    c.info = p.info;
    c.info_foreground = p.on_solid;
    c.link = p.text2;
    c.link_hover = p.text;
    c.sidebar = p.side;
    c.sidebar_border = p.line;
    c.sidebar_accent = p.well;
    c.sidebar_accent_foreground = p.text;
    c.sidebar_foreground = p.text;
    c.title_bar = clear;
    c.title_bar_border = clear;
    c.window_border = clear;
    c.skeleton = p.well;
    c.scrollbar = clear;
    c.scrollbar_thumb = p.text.alpha(0.30);
    c.scrollbar_thumb_hover = p.text.alpha(0.42);
    c.switch = p.edge;
    c.switch_thumb = p.on_solid;
    c.progress_bar = p.text;
    c.tab_active = p.well;
    c.drag_border = p.info;
    c.drop_target = p.info_wash;
    cx.set_global(palette);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG relative luminance of an opaque colour.
    fn luminance(c: Hsla) -> f32 {
        let rgb = c.to_rgb();
        let lin = |v: f32| {
            if v <= 0.039_28 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(rgb.r) + 0.7152 * lin(rgb.g) + 0.0722 * lin(rgb.b)
    }

    fn contrast(a: Hsla, b: Hsla) -> f32 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn every_text_step_stays_well_clear_of_every_surface_it_sits_on() {
        for p in [Palette::dark(), Palette::light()] {
            for (name, ground) in [
                ("win", p.win),
                ("side", p.side),
                ("surf", p.surf),
                ("well", p.well),
            ] {
                assert_eq!(ground.a, 1.0, "{name} is opaque: the window has no glass");
                assert!(contrast(p.text, ground) > 10.0, "text on {name}: {p:?}");
                assert!(contrast(p.text2, ground) > 4.5, "text2 on {name}: {p:?}");
                // The label step is the quietest text and still body-legible.
                assert!(contrast(p.text3, ground) > 4.5, "text3 on {name}: {p:?}");
            }
            assert!(contrast(p.on_solid, p.solid) > 10.0, "{p:?}");
            // The caption under the deck's picture sits on the stage.
            assert!(contrast(p.text2, p.stage) > 4.5, "{p:?}");
        }
    }

    #[test]
    fn a_controls_edge_reads_without_colour() {
        // The edge is the only thing that outlines a secondary button, a
        // switch that is off and a popup, so it carries 3:1 against what
        // those sit on: the window and a card.
        for p in [Palette::dark(), Palette::light()] {
            assert!(contrast(p.edge, p.win) >= 3.0, "edge on win: {p:?}");
            assert!(contrast(p.edge, p.surf) >= 3.0, "edge on surf: {p:?}");
        }
        // Against a well it is 3:1 in dark and 2.9:1 in light, as the design
        // draws it. Where an edge rims a well (a popup, a keycap, a field)
        // the same edge meets the card outside at 3.4:1, so the control's
        // outline still reads.
        assert!(contrast(Palette::dark().edge, Palette::dark().well) >= 3.0);
        assert!(contrast(Palette::light().edge, Palette::light().well) >= 2.9);
    }

    #[test]
    fn colour_is_kept_for_state_and_selection_is_the_neutral_outline() {
        for p in [Palette::dark(), Palette::light()] {
            // Selection and the primary are greys: no hue at all.
            for (name, grey) in [("solid", p.solid), ("outline", p.outline), ("edge", p.edge)] {
                assert!(grey.s < 0.12, "{name} has no hue: {grey:?}");
            }
            // A line is ink at an alpha, so it reads the same on every
            // surface; the washes are their hue at an alpha.
            assert!(p.line.a < 0.2);
            for wash in [p.warn_wash, p.bad_wash, p.info_wash] {
                assert!(wash.a > 0.05 && wash.a < 0.25, "{wash:?}");
            }
            assert!(
                p.warn_line.a > p.warn_wash.a,
                "the rim is stronger than its wash"
            );
            // A deck's key is the deck's, not the window's.
            assert_eq!(p.key_fill, hex(0x0B0B0D));
            assert_eq!(p.key_line, hex(0x2C2E33));
        }
        // The amber wash is the bright amber's in light too.
        let (dark, light) = (Palette::dark(), Palette::light());
        assert_eq!(light.warn_wash.h, dark.warn.h);
        assert_ne!(light.warn.to_rgb(), dark.warn.to_rgb());
    }

    #[test]
    fn the_firmwares_five_tones_stay_five_different_colours() {
        for p in [Palette::dark(), Palette::light()] {
            let tones = [Tone::Blue, Tone::Amber, Tone::Green, Tone::Red, Tone::Gray];
            for (i, a) in tones.iter().enumerate() {
                for b in &tones[i + 1..] {
                    assert_ne!(p.tone(*a), p.tone(*b), "{a:?} and {b:?}");
                    assert_ne!(p.tone_text(*a), p.tone_text(*b), "{a:?} and {b:?}");
                }
            }
            // Healthy and neutral are a dot and a word; the rest a pill.
            assert_eq!(p.tone_wash(Tone::Green), None);
            assert_eq!(p.tone_wash(Tone::Gray), None);
            assert_eq!(p.tone_wash(Tone::Amber), Some(p.warn_wash));
            assert_eq!(p.tone_wash(Tone::Red), Some(p.bad_wash));
            assert_eq!(p.tone_wash(Tone::Blue), Some(p.info_wash));
            assert_eq!(p.tone(Tone::Gray), p.edge, "the neutral dot is an edge");
            // A word on a wash keeps 4.5:1 against the wash over a card.
            for tone in [Tone::Amber, Tone::Red, Tone::Blue] {
                let wash = p.tone_wash(tone).unwrap();
                let ground = p.surf.blend(wash);
                assert!(contrast(p.tone_text(tone), ground) > 4.5, "{tone:?}: {p:?}");
            }
        }
    }

    #[test]
    fn the_window_font_puts_the_languages_own_cjk_family_first_and_keeps_all_three() {
        assert_eq!(
            cjk_fallbacks(Language::English),
            ["PingFang SC", "PingFang TC", "Hiragino Sans"]
        );
        assert_eq!(
            cjk_fallbacks(Language::SimplifiedChinese),
            ["PingFang SC", "PingFang TC", "Hiragino Sans"]
        );
        assert_eq!(
            cjk_fallbacks(Language::TraditionalChinese),
            ["PingFang TC", "PingFang SC", "Hiragino Sans"]
        );
        assert_eq!(
            cjk_fallbacks(Language::Japanese),
            ["Hiragino Sans", "PingFang SC", "PingFang TC"]
        );
        for language in Language::ALL {
            let font = ui_font(language);
            assert_eq!(font.family.as_ref(), SANS);
            assert_eq!(font.weight, FontWeight::NORMAL);
            let fallbacks = font.fallbacks.expect("a CJK list");
            assert_eq!(
                fallbacks.fallback_list(),
                cjk_fallbacks(language).as_slice()
            );
        }
    }
}
