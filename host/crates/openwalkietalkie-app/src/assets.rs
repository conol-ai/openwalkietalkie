//! The app's [`AssetSource`] (§8.1's `Application::new().with_assets`).
//!
//! Every icon the window draws is an SVG embedded here: Solar's Linear set
//! (480 Design, CC BY 4.0) on a 24 px grid with 1.5 px strokes, the same
//! subset Zeron ships, plus a handful drawn in the same style where Solar
//! has nothing (the deck and its two states, the dial, the plug, Bluetooth,
//! the Mac's modifier marks, the timer plate's ring) and the agents' own
//! marks. `assets/NOTICE` carries the attributions.
//!
//! The deck's picture is raster: one PNG per case colour and appearance,
//! with and without a lit PTT key, drawn from the design's own deck at three
//! times the size the window shows it, with the shadow in a clear margin.
//! [`DEVICE`] serves them, the splash first run puts on the deck's screen
//! and the brand tile, under `device/`. An image path needs its `/`: gpui
//! fetches a path without one as a URL.
//!
//! gpui-component draws some furniture of its own — a notification's
//! leading glyph, a checkbox tick, a close button — and asks for those by
//! Lucide's names (`icons/info.svg`, `icons/circle-x.svg`). [`ALIASES`]
//! answers those with the Solar glyph that means the same, so its widgets
//! and the window's own are one family. Anything else under `icons/`
//! resolves to [`FALLBACK`], a small neutral dot, instead of logging a
//! missing asset on every frame.
//!
//! The fonts are **not** here: they are registered with the text system
//! straight from the binary (`style::register_fonts`).
use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

/// What an icon this app does not carry draws: a dot, which is honest about
/// being a placeholder and costs one path.
pub const FALLBACK: &str = concat!(
    r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" "#,
    r#"stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="2.5"/></svg>"#
);

macro_rules! embed {
    ($($name:literal),+ $(,)?) => {
        &[$((
            concat!("icons/", $name, ".svg"),
            include_str!(concat!("../assets/icons/", $name, ".svg")),
        )),+]
    };
}

macro_rules! raster {
    ($($name:literal),+ $(,)?) => {
        &[$((
            concat!("device/", $name, ".png"),
            include_bytes!(concat!("../assets/device/", $name, ".png")).as_slice(),
        )),+]
    };
}

/// Every icon, by the path `svg().path(..)` asks for.
pub const ICONS: &[(&str, &str)] = embed![
    "action-play",
    "add-circle",
    "alt-arrow-down",
    "alt-arrow-left",
    "alt-arrow-right",
    "alt-arrow-up",
    "archive-minimalistic",
    "arrow-right",
    "arrow-up-right",
    "arrow-up",
    "bell",
    "bluetooth",
    "bot",
    "chat-round-line",
    "check-circle",
    "check",
    "checklist",
    "claude-mark",
    "clock-circle",
    "close-circle",
    "close",
    "copy",
    "cohdl-mark",
    "conol-mark",
    "danger-triangle",
    "deck-care",
    "deck-off",
    "deck",
    "dial",
    "document",
    "eye-closed",
    "eye",
    "folder-with-files",
    "folder",
    "gemini-mark",
    "global",
    "hard-drive",
    "home",
    "info-circle",
    "key-minimalistic",
    "keyboard",
    "laptop",
    "list",
    "logout-2",
    "magic-stick-3",
    "magnifer",
    "microphone",
    "mod-command",
    "mod-control",
    "mod-option",
    "mod-shift",
    "monitor",
    "moon",
    "more-horizontal",
    "openai-mark",
    "pin",
    "plate-ring-half",
    "plate-ring",
    "plug",
    "plus",
    "refresh",
    "return",
    "server",
    "settings",
    "sidebar-minimalistic-left",
    "smartphone",
    "star-bold",
    "star",
    "stop",
    "sun",
    "tag",
    "terminal",
    "trash-bin-minimalistic",
    "tuning",
    "volume-loud",
    "widget",
    "wifi-off",
];

/// The deck's picture in each case colour and appearance (`-ptt`: the PTT
/// key lit, for first run), the splash on its screen in first run, and the
/// brand tile, by the path `img(..)` asks for.
pub const DEVICE: &[(&str, &[u8])] = raster![
    "brand-tile",
    "deck-black-dark-ptt",
    "deck-black-dark",
    "deck-black-light-ptt",
    "deck-black-light",
    "deck-splash",
    "deck-white-dark-ptt",
    "deck-white-dark",
    "deck-white-light-ptt",
    "deck-white-light",
];

/// gpui-component's names for the glyphs it draws itself, and the icon in
/// [`ICONS`] each one is drawn with.
pub const ALIASES: &[(&str, &str)] = &[
    ("icons/info.svg", "icons/info-circle.svg"),
    ("icons/circle-check.svg", "icons/check-circle.svg"),
    ("icons/circle-x.svg", "icons/close-circle.svg"),
    ("icons/triangle-alert.svg", "icons/danger-triangle.svg"),
];

/// The bundle. `load` never fails and never returns `None` for an
/// `icons/` path: see [`FALLBACK`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Assets;

impl Assets {
    /// The bytes for `path`, or the fallback glyph for an unknown `icons/…`.
    pub fn get(path: &str) -> Option<&'static str> {
        let path = ALIASES
            .iter()
            .find(|(alias, _)| *alias == path)
            .map_or(path, |(_, real)| real);
        if let Some((_, svg)) = ICONS.iter().find(|(name, _)| *name == path) {
            return Some(svg);
        }
        path.starts_with("icons/").then_some(FALLBACK)
    }

    /// The picture at `path` under `device/`, if this app carries it. An
    /// unknown one is absent, not a placeholder: a missing picture is a bug
    /// to see, where a missing glyph is a dot.
    pub fn picture(path: &str) -> Option<&'static [u8]> {
        DEVICE
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, png)| *png)
    }
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(png) = Assets::picture(path) {
            return Ok(Some(Cow::Borrowed(png)));
        }
        Ok(Assets::get(path).map(|svg| Cow::Borrowed(svg.as_bytes())))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .map(|(name, _)| *name)
            .chain(ALIASES.iter().map(|(alias, _)| *alias))
            .chain(DEVICE.iter().map(|(name, _)| *name))
            .filter(|name| name.starts_with(path))
            .map(SharedString::from)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{ic, Mod, BRAND_TILE};
    use gpui_component::{IconName, IconNamed};

    #[test]
    fn every_embedded_icon_is_a_well_formed_svg_under_a_unique_path() {
        assert!(!ICONS.is_empty());
        for (i, (path, svg)) in ICONS.iter().enumerate() {
            assert!(
                path.starts_with("icons/") && path.ends_with(".svg"),
                "{path}"
            );
            assert!(svg.starts_with("<svg "), "{path}");
            assert!(svg.ends_with("</svg>"), "{path}");
            assert!(svg.contains("currentColor"), "{path} must be tintable");
            assert!(
                !ICONS[i + 1..].iter().any(|(other, _)| other == path),
                "{path} is embedded twice"
            );
        }
        assert!(FALLBACK.starts_with("<svg ") && FALLBACK.ends_with("</svg>"));
    }

    #[test]
    fn gpui_components_own_glyphs_are_answered_with_the_windows_icons() {
        // `gpui_component` draws these itself (a notification's leading
        // glyph, the close button, the checkbox tick); an app that does not
        // carry them logs a missing asset on every frame they are up.
        for name in [
            IconName::Close,
            IconName::Check,
            IconName::Info,
            IconName::CircleCheck,
            IconName::CircleX,
            IconName::TriangleAlert,
        ] {
            let path = name.path();
            let svg = Assets::get(path.as_ref());
            assert!(
                svg.is_some() && svg != Some(FALLBACK),
                "{path} is not carried"
            );
        }
        for (alias, real) in ALIASES {
            assert!(
                ICONS.iter().any(|(path, _)| path == real),
                "{alias} points at {real}, which is not embedded"
            );
            assert!(
                !ICONS.iter().any(|(path, _)| path == alias),
                "{alias} is both an alias and an icon"
            );
        }
    }

    #[test]
    fn every_icon_the_window_names_is_embedded() {
        let modifiers = Mod::ALL.map(Mod::icon);
        for path in [
            ic::ADD,
            ic::ARCHIVE,
            ic::ARROW_RIGHT,
            ic::ARROW_UP,
            ic::BELL,
            ic::BLUETOOTH,
            ic::BOT,
            ic::CHAT,
            ic::CHECK,
            ic::CHECK_CIRCLE,
            ic::CHEVRON_DOWN,
            ic::CHEVRON_LEFT,
            ic::CHEVRON_RIGHT,
            ic::CHEVRON_UP,
            ic::CLAUDE,
            ic::CLOCK,
            ic::CLOSE,
            ic::CLOSE_CIRCLE,
            ic::CONOL,
            ic::COPY,
            ic::DANGER,
            ic::DECK,
            ic::DECK_CARE,
            ic::DECK_OFF,
            ic::DIAL,
            ic::DOCUMENT,
            ic::EXTERNAL,
            ic::EYE,
            ic::EYE_CLOSED,
            ic::FOLDER,
            ic::GEMINI,
            ic::GLOBAL,
            ic::INFO,
            ic::KEY,
            ic::KEYBOARD,
            ic::LAPTOP,
            ic::LIST,
            ic::LOGOUT,
            ic::MAGIC,
            ic::MICROPHONE,
            ic::MOD_COMMAND,
            ic::MOD_CONTROL,
            ic::MOD_OPTION,
            ic::MOD_SHIFT,
            ic::MONITOR,
            ic::OPENAI,
            ic::PIN,
            ic::PLATE_RING,
            ic::PLATE_RING_HALF,
            ic::PLAY,
            ic::PLUG,
            ic::PLUS,
            ic::REFRESH,
            ic::RETURN,
            ic::SERVER,
            ic::SETTINGS,
            ic::SMARTPHONE,
            ic::STAR,
            ic::STOP,
            ic::TAG,
            ic::TERMINAL,
            ic::TRASH,
            ic::TUNING,
            ic::VOLUME,
            ic::WIDGET,
            ic::WIFI_OFF,
        ]
        .into_iter()
        .chain(modifiers)
        {
            let svg = Assets::get(path);
            assert!(
                svg.is_some() && svg != Some(FALLBACK),
                "{path} is drawn but not embedded"
            );
        }
    }

    #[test]
    fn the_drawn_marks_are_the_designs_strokes_on_their_own_grids() {
        // The deck glyph and its two states share one drawing on the 24 px
        // grid at 1.5; the states only add to it.
        let deck = Assets::get(ic::DECK).unwrap();
        assert!(deck.contains(r#"viewBox="0 0 24 24""#) && deck.contains(r#"stroke-width="1.5""#));
        let body = deck.trim_end_matches("</svg>");
        for state in [ic::DECK_OFF, ic::DECK_CARE] {
            let svg = Assets::get(state).unwrap();
            assert!(svg.starts_with(body) && svg.len() > deck.len(), "{state}");
        }
        // The modifier marks are on a 16 px grid at 1.4.
        for m in Mod::ALL {
            let svg = Assets::get(m.icon()).unwrap();
            assert!(
                svg.contains(r#"viewBox="0 0 16 16""#) && svg.contains(r#"stroke-width="1.4""#),
                "{m:?}"
            );
        }
        // The timer's ring and its lit half are one 54 px ring, 6 thick.
        for ring in [ic::PLATE_RING, ic::PLATE_RING_HALF] {
            let svg = Assets::get(ring).unwrap();
            assert!(
                svg.contains(r#"viewBox="0 0 54 54""#) && svg.contains(r#"stroke-width="6""#),
                "{ring}"
            );
        }
    }

    /// A PNG's width and height, from its header.
    fn png_size(png: &[u8]) -> (u32, u32) {
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
        assert_eq!(&png[12..16], b"IHDR");
        let word = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
        (word(16), word(20))
    }

    #[test]
    fn every_picture_loads_at_three_times_the_size_it_is_drawn() {
        let assets = Assets;
        for (path, _) in DEVICE {
            assert!(
                path.starts_with("device/") && path.ends_with(".png"),
                "{path}"
            );
            let png = assets.load(path).unwrap().expect(path);
            let size = png_size(&png);
            match *path {
                // The 22 px tile beside the app's name.
                BRAND_TILE => assert_eq!(size, (66, 66)),
                // The splash on a 149 px screen, under twice its size.
                "device/deck-splash.png" => assert_eq!(size, (480, 480)),
                // The 196×315 deck in a 256×375 box with room for its shadow.
                _ => assert_eq!(size, (768, 1125), "{path}"),
            }
        }
        // Both case colours, both appearances, with and without the lit key.
        for case in ["white", "black"] {
            for appearance in ["dark", "light"] {
                for lit in ["", "-ptt"] {
                    let path = format!("device/deck-{case}-{appearance}{lit}.png");
                    assert!(Assets::picture(&path).is_some(), "{path}");
                }
            }
        }
        assert_eq!(DEVICE.len(), 10);
        // A picture this app does not carry is absent, not a placeholder.
        assert_eq!(Assets::picture("device/deck-red-dark.png"), None);
        assert!(assets.load("device/deck-red-dark.png").unwrap().is_none());
        assert!(assets.load("deck-white-dark.png").unwrap().is_none());
        assert_eq!(
            assets.list("device/").unwrap().len(),
            DEVICE.len(),
            "every picture is listed"
        );
    }

    #[test]
    fn an_icon_this_app_does_not_carry_falls_back_instead_of_going_missing() {
        let close = Assets::get("icons/close.svg");
        assert!(close.is_some() && close != Some(FALLBACK));
        assert_eq!(Assets::get("icons/chevron-down.svg"), Some(FALLBACK));
        assert_eq!(
            Assets::get(IconName::Loader.path().as_ref()),
            Some(FALLBACK)
        );
        // Not an icon at all: absent, so gpui can say so once.
        assert_eq!(Assets::get("fonts/Geist.ttf"), None);
        let assets = Assets;
        assert!(assets.load("icons/anything.svg").unwrap().is_some());
        assert!(assets.load("nope").unwrap().is_none());
        assert_eq!(
            assets.list("icons/").unwrap().len(),
            ICONS.len() + ALIASES.len()
        );
        assert!(assets.list("fonts/").unwrap().is_empty());
    }
}
