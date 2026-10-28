//! The app's [`AssetSource`] (§8.1's `Application::new().with_assets`).
//!
//! Every icon the window draws is an SVG embedded here: Solar's Linear set
//! (480 Design, CC BY 4.0) on a 24 px grid with 1.5 px strokes, the same
//! subset Zeron ships, plus a handful drawn in the same style where Solar
//! has nothing (the dial, the plug, Bluetooth) and the agents' own marks.
//! `assets/NOTICE` carries the attributions.
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
//! straight from the binary (`style::register_fonts`), so this source stays
//! icons only.
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
    "monitor",
    "moon",
    "more-horizontal",
    "openai-mark",
    "pin",
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
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == "branding/radio.png" {
            return Ok(Some(Cow::Borrowed(include_bytes!("../../../../assets/branding/radio.png"))));
        }
        Ok(Assets::get(path).map(|svg| Cow::Borrowed(svg.as_bytes())))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .map(|(name, _)| *name)
            .chain(ALIASES.iter().map(|(alias, _)| *alias))
            .filter(|name| name.starts_with(path))
            .map(SharedString::from)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::ic;
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
        for path in [
            ic::ADD,
            ic::ARCHIVE,
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
            ic::COPY,
            ic::DANGER,
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
            ic::MONITOR,
            ic::OPENAI,
            ic::PIN,
            ic::PLAY,
            ic::PLUG,
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
            ic::WIFI_OFF,
        ] {
            assert!(
                ICONS.iter().any(|(p, _)| *p == path),
                "{path} is drawn but not embedded"
            );
        }
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
