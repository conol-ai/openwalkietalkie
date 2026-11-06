//! The agent marks the roster draws for each session: the same marks as the
//! Mac app (its `claude-mark`, `openai-mark`, `gemini-mark` and
//! `conol-mark`; Lucide's bot for anything else), as alpha masks from
//! `tools/generate-agent-marks.py --all`, tinted here. Two sizes: 24 px for
//! the carousel's peeks, 40 px for its card.

use crate::deck::msg::AgentKind;
use crate::display::Band;
use crate::ui::{kit, theme};

/// The sizes there are masks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Size {
    Small,
    Large,
}

impl Size {
    #[must_use]
    pub const fn px(self) -> i32 {
        match self {
            Size::Small => 24,
            Size::Large => 40,
        }
    }
}

const SMALL: usize = 24 * 24;
const LARGE: usize = 40 * 40;

const CLAUDE: &[u8; SMALL] = include_bytes!("../../assets/marks/claude.alpha");
const CODEX: &[u8; SMALL] = include_bytes!("../../assets/marks/codex.alpha");
const GEMINI: &[u8; SMALL] = include_bytes!("../../assets/marks/gemini.alpha");
const CONOL: &[u8; SMALL] = include_bytes!("../../assets/marks/conol.alpha");
const OTHER: &[u8; SMALL] = include_bytes!("../../assets/marks/other.alpha");
const CLAUDE_L: &[u8; LARGE] = include_bytes!("../../assets/marks/claude-40.alpha");
const CODEX_L: &[u8; LARGE] = include_bytes!("../../assets/marks/codex-40.alpha");
const GEMINI_L: &[u8; LARGE] = include_bytes!("../../assets/marks/gemini-40.alpha");
const CONOL_L: &[u8; LARGE] = include_bytes!("../../assets/marks/conol-40.alpha");
const OTHER_L: &[u8; LARGE] = include_bytes!("../../assets/marks/other-40.alpha");

/// A kind's tint: Claude's clay, Conol's yellow, Gemini's blue, Codex and
/// everything else in the text colours.
#[must_use]
pub const fn tint(kind: AgentKind) -> u32 {
    match kind {
        AgentKind::Claude => 0xD97757,
        AgentKind::Codex => 0xE8E9EA,
        AgentKind::Gemini => 0x4C9AFF,
        AgentKind::Conol => 0xFFE238,
        AgentKind::Other => 0xC9CCD1,
    }
}

/// A kind's mask at `size`.
#[must_use]
pub fn mask(kind: AgentKind, size: Size) -> &'static [u8] {
    match (kind, size) {
        (AgentKind::Claude, Size::Small) => CLAUDE,
        (AgentKind::Codex, Size::Small) => CODEX,
        (AgentKind::Gemini, Size::Small) => GEMINI,
        (AgentKind::Conol, Size::Small) => CONOL,
        (AgentKind::Other, Size::Small) => OTHER,
        (AgentKind::Claude, Size::Large) => CLAUDE_L,
        (AgentKind::Codex, Size::Large) => CODEX_L,
        (AgentKind::Gemini, Size::Large) => GEMINI_L,
        (AgentKind::Conol, Size::Large) => CONOL_L,
        (AgentKind::Other, Size::Large) => OTHER_L,
    }
}

/// Draw `kind`'s mark with its top-left corner at (`x`, `y`); `dim` greys it
/// (a stale or offline session).
pub fn draw(band: &mut Band, kind: AgentKind, size: Size, x: i32, y: i32, dim: bool) {
    let px = size.px();
    let mask = mask(kind, size);
    let color = if dim { theme::GRAY } else { tint(kind) };
    for row in 0..px {
        for col in 0..px {
            let a = mask[(row * px + col) as usize];
            if a != 0 {
                band.blend_px(x + col, y + row, color, a);
            }
        }
    }
}

/// A session's status as a presence dot on its mark.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Presence {
    /// Running: filled green.
    Running,
    /// Waiting for the operator: filled amber.
    Waiting,
    /// Failed: filled red.
    Failed,
    /// Not running: a hollow ring.
    Stopped,
}

/// The dot's radius at `size`.
#[must_use]
pub const fn dot_radius(size: Size) -> i32 {
    match size {
        Size::Small => 5,
        Size::Large => 6,
    }
}

/// The mark with its presence dot on the lower right, ringed in `bg` so it
/// reads against the mark.
pub fn draw_with_dot(band: &mut Band, kind: AgentKind, size: Size, x: i32, y: i32, presence: Presence, bg: u32, dim: bool) {
    draw(band, kind, size, x, y, dim);
    let px = size.px();
    let r = dot_radius(size);
    let (cx, cy) = (x + px - r + 2, y + px - r + 2);
    kit::dot(band, cx, cy, r + 3, bg);
    let color = match (presence, dim) {
        (_, true) => None,
        (Presence::Running, _) => Some(theme::GREEN),
        (Presence::Waiting, _) => Some(theme::AMBER),
        (Presence::Failed, _) => Some(theme::RED),
        (Presence::Stopped, _) => None,
    };
    match color {
        Some(color) => kit::dot(band, cx, cy, r, color),
        None => {
            kit::dot(band, cx, cy, r, if dim { theme::OUTLINE_THUMB } else { theme::TEXT_FAINT });
            kit::dot(band, cx, cy, r - 2, bg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_a_visible_mark_at_both_sizes() {
        for size in [Size::Small, Size::Large] {
            let len = (size.px() * size.px()) as usize;
            for kind in [AgentKind::Claude, AgentKind::Codex, AgentKind::Gemini, AgentKind::Conol, AgentKind::Other] {
                let mask = mask(kind, size);
                assert_eq!(mask.len(), len);
                let ink = mask.iter().filter(|&&a| a > 128).count();
                assert!(ink > len / 14 && ink < len * 3 / 4, "{kind:?} {size:?}: {ink}");
            }
        }
    }
}
