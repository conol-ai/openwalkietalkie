//! Context-aware dictation (`[context]` in `config.toml`): a dictated
//! sentence rewritten into what the owner would have typed in the focused
//! app, by two models the owner configures — one that names the scene from
//! the focused app's name and window title (text only, never a screenshot
//! or the window's contents: [`focus`]), and one that edits the text
//! ([`prompt`], [`http`]).
//!
//! **Dictation at the cursor only.** The core holds a final transcript
//! addressed to the Dictate agent ([`is_dictate`]) while [`rewrite`] works
//! on it, and releases it rewritten or exactly as heard, in order, within
//! the budget (`Core::on_from_audio`). Off — the default, and a config with
//! no `[context]` table — nothing here runs.
pub mod api;
pub mod focus;
pub mod http;
pub mod keys;
pub mod prompt;
pub mod rewrite;

/// Whether an agent's session key names a Dictate agent: the Omarchy
/// bridge's `omarchy:<host>:dictate`, or any companion's `<…>:dictate`.
#[must_use]
pub fn is_dictate(session_key: &str) -> bool {
    session_key.ends_with(":dictate")
}

#[cfg(test)]
mod tests {
    #[test]
    fn dictate_is_known_by_its_session_key() {
        assert!(super::is_dictate("omarchy:tony-nuc7:dictate"));
        assert!(!super::is_dictate("claude:4b1d"));
        assert!(!super::is_dictate("omarchy:dictate-later"));
    }
}
