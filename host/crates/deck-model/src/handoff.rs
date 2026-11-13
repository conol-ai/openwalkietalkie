//! The hand-off (the companion design's decision 8): a press on a session's
//! `Open app` card brings forward the app the session runs in.
//!
//! It is a deliberate act and nothing else is one. ENTER (or a touch, or a
//! dictated answer) on a `deliver: focus` card asks for it
//! (`DesktopCmd::Focus`), and so does `op.focus`. A deck `channel` (opening
//! a session, moving between sessions or cards, a PTT press) moves the focus
//! and raises nothing on the computer: dictating to a session in the
//! background must not pull its app forward.
//!
//! What the deck shows for a hand-off lives here, in the deck's four
//! languages: the card's one choice, and the line for a hand-off that could
//! not happen. The firmware shows host text as it comes, so the daemon picks
//! the language, as it does for the care test card.
use crate::{FailureLine, Snapshot};

/// The one choice of a `focus` card, in the order of the deck's `language`
/// setting: English, 简体中文, 繁體中文, 日本語.
pub const OPEN_APP: [&str; 4] = ["Open app", "打开应用", "打開應用", "アプリを開く"];

/// `OPEN_APP` in the language the deck shows.
#[must_use]
pub fn open_app(snapshot: &Snapshot) -> &'static str {
    OPEN_APP[crate::orders::language_of(snapshot)]
}

/// Why a hand-off did not happen. Each one is shown on the deck the way a
/// failed order is (`Store::fail_on_deck`), under its own reason word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandoffFailure {
    /// The session named no app the daemon may raise: its hook helper found
    /// none (an `ssh` session, a `tmux` server `launchd` started), or it is
    /// not a hook session at all.
    NoApp,
    /// The app is not running. A hand-off never launches one.
    NotRunning,
    /// macOS did not bring the app forward.
    Refused,
}

impl HandoffFailure {
    /// The word `card.set{host:"failure", reason}` carries.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            HandoffFailure::NoApp => "no_app",
            HandoffFailure::NotRunning => "app_not_running",
            HandoffFailure::Refused => "raise_refused",
        }
    }

    /// What the deck says. Never the toast: firmware 0.13.0's toast reads
    /// "Transcription failed", which a hand-off is not.
    #[must_use]
    pub const fn line(self) -> FailureLine {
        let text = match self {
            HandoffFailure::NoApp => [
                "Not opened: no app is known for this session.",
                "未打开：不知道此会话所在的应用。",
                "未打開：不知道此工作階段所在的應用程式。",
                "開けませんでした：このセッションのアプリが分かりません。",
            ],
            HandoffFailure::NotRunning => [
                "Not opened: its app is not running.",
                "未打开：其应用未在运行。",
                "未打開：其應用程式未在執行。",
                "開けませんでした：アプリが起動していません。",
            ],
            HandoffFailure::Refused => [
                "Not opened: macOS did not bring its app forward.",
                "未打开：macOS 未能切换到其应用。",
                "未打開：macOS 未能切至其應用程式。",
                "開けませんでした：macOS がアプリを前面へ出しませんでした。",
            ],
        };
        FailureLine {
            text,
            nothing_sent: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The words follow the deck's own language setting, and a deck whose
    /// hello said it has none is spoken to in English.
    #[test]
    fn the_choice_is_in_the_decks_language() {
        let mut snapshot = Snapshot::default();
        assert_eq!(open_app(&snapshot), "Open app");
        snapshot.settings.language = 1;
        assert_eq!(open_app(&snapshot), "打开应用");
        snapshot.settings.language = 2;
        assert_eq!(open_app(&snapshot), "打開應用");
        snapshot.settings.language = 3;
        assert_eq!(open_app(&snapshot), "アプリを開く");
        snapshot.device.fw = Some("0.12.0".into());
        snapshot.device.language_supported = false;
        assert_eq!(open_app(&snapshot), "Open app", "no language setting");
    }

    /// Every failure has a reason word the record accepts, a line in each
    /// language, and never claims the toast's "nothing was sent".
    #[test]
    fn every_failure_has_a_word_and_four_lines() {
        for failure in [
            HandoffFailure::NoApp,
            HandoffFailure::NotRunning,
            HandoffFailure::Refused,
        ] {
            let reason = failure.reason();
            assert!(
                (1..=32).contains(&reason.len())
                    && reason.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{reason}"
            );
            let line = failure.line();
            assert!(!line.nothing_sent, "{failure:?}");
            assert!(line.text.iter().all(|t| !t.trim().is_empty()));
            assert!(line.text[0].starts_with("Not opened: "), "{failure:?}");
        }
    }
}
