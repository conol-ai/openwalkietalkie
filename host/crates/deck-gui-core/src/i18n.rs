//! Desktop interface copy. Translation is explicit: host/user content never
//! passes through an automatic renderer or substring replacement.
#[rustfmt::skip]
#[path = "../../../../src/language.rs"]
mod language;
pub use language::Language;
mod app;
mod auxiliary;
mod common;
mod platform;

use std::sync::OnceLock;

/// English source template, Simplified Chinese, Traditional Chinese, Japanese.
pub type Entry = (&'static str, &'static str, &'static str, &'static str);

fn tables() -> [&'static [Entry]; 4] {
    [common::STRINGS, app::STRINGS, auxiliary::STRINGS, platform::STRINGS]
}

/// The app's own tables, installed once by [`extend`].
static EXTENDED: OnceLock<&'static [&'static [Entry]]> = OnceLock::new();

/// Every table a lookup scans, in order: the four built-in ones first, so
/// their words win, then the app's.
fn scanned() -> impl Iterator<Item = &'static [Entry]> {
    tables()
        .into_iter()
        .chain(EXTENDED.get().into_iter().flat_map(|app| app.iter().copied()))
}

/// Add the app's tables, scanned after the four built-in ones, so a key the
/// built-in tables already have keeps its words. Once per process: `false`
/// when tables were already added, and the first ones stay.
pub fn extend(tables: &'static [&'static [Entry]]) -> bool {
    EXTENDED.set(tables).is_ok()
}

/// Whether `english` is a key of any table. Not the same as a translation
/// that differs from the English: a few rows are the same in all four
/// languages on purpose.
pub fn has_entry(english: &str) -> bool {
    scanned().any(|table| table.iter().any(|entry| entry.0 == english))
}

/// Every row of every table, built-in first, for the tests that check them
/// all.
pub fn rows() -> Vec<Entry> {
    scanned().flat_map(|table| table.iter().copied()).collect()
}

impl Language {
    pub fn text(self, english: &str) -> &str {
        // Windows reads every row through `on_this_computer`, English too.
        if self == Self::English && cfg!(not(windows)) { return english; }
        for table in scanned() {
            if let Some(entry) = table.iter().find(|entry| entry.0 == english) {
                let words = match self {
                    Self::English => entry.0,
                    Self::SimplifiedChinese => entry.1,
                    Self::TraditionalChinese => entry.2,
                    Self::Japanese => entry.3,
                };
                return on_this_computer(words);
            }
        }
        english
    }

    pub fn format(self, english: &str, values: &[&str]) -> String {
        render(self.text(english), |key| key.parse::<usize>().ok().and_then(|index| values.get(index).copied()))
    }

    pub fn format_named(self, english: &str, values: &[(&str, &str)]) -> String {
        render(self.text(english), |key| values.iter().find(|(name, _)| *name == key).map(|(_, value)| *value))
    }
}

/// A row's words as this computer says them. The copy names the Mac it was
/// written for ("on this Mac", "此 Mac 上", "この Mac"); on Windows each
/// whole word `Mac` is `PC`, never a `Mac` inside a word (Macropad, macOS).
/// Only a table's own row comes here, never a key no table has: that may be
/// host or user content. Each row is changed once and kept.
#[cfg(windows)]
fn on_this_computer(words: &'static str) -> &'static str {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static CHANGED: OnceLock<Mutex<HashMap<&'static str, &'static str>>> = OnceLock::new();
    if !words.contains("Mac") {
        return words;
    }
    let mut changed = CHANGED.get_or_init(Mutex::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    changed.entry(words).or_insert_with(|| Box::leak(pc_for_mac(words).into_boxed_str()))
}

#[cfg(not(windows))]
fn on_this_computer(words: &'static str) -> &'static str {
    words
}

/// `text` with every whole word `Mac` (no ASCII letter or digit on either
/// side) as `PC`.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn pc_for_mac(text: &str) -> String {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("Mac") {
        let before = rest[..at].chars().next_back().or_else(|| out.chars().next_back());
        let after = rest[at + 3..].chars().next();
        out.push_str(&rest[..at]);
        out.push_str(if word(before) || word(after) { "Mac" } else { "PC" });
        rest = &rest[at + 3..];
    }
    out.push_str(rest);
    out
}

/// Substitute only the original template, never text supplied as a value.
fn render<'a>(template: &str, mut value: impl FnMut(&str) -> Option<&'a str>) -> String {
    let mut result = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        result.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            result.push_str(&rest[open..]);
            return result;
        };
        if let Some(replacement) = value(&after[..close]) {
            result.push_str(replacement);
        } else {
            result.push_str(&rest[open..open + close + 2]);
        }
        rest = &after[close + 1..];
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_windows_the_word_mac_is_pc_and_a_word_containing_it_is_not() {
        assert_eq!(pc_for_mac("What this app has added to your Mac"), "What this app has added to your PC");
        assert_eq!(pc_for_mac("on this Mac."), "on this PC.");
        assert_eq!(pc_for_mac("Mac"), "PC");
        assert_eq!(pc_for_mac("在此 Mac 上识别"), "在此 PC 上识别");
        assert_eq!(pc_for_mac("このアプリが Mac に追加したもの"), "このアプリが PC に追加したもの");
        assert_eq!(pc_for_mac("Macropad keys, macOS, iMac, Mac2"), "Macropad keys, macOS, iMac, Mac2");
        assert_eq!(pc_for_mac("Mac, Mac\nMac"), "PC, PC\nPC");
    }

    fn placeholders(text: &str) -> std::collections::BTreeSet<&str> {
        let mut found = std::collections::BTreeSet::new();
        let mut rest = text;
        while let Some(open) = rest.find('{') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('}') else { break };
            found.insert(&after[..close]);
            rest = &after[close + 1..];
        }
        found
    }
    #[test]
    fn every_entry_has_four_consistent_translations_and_placeholders() {
        for table in tables() {
            for &(english, hans, hant, japanese) in table {
                for (language, text) in [(Language::English, english), (Language::SimplifiedChinese, hans), (Language::TraditionalChinese, hant), (Language::Japanese, japanese)] {
                    assert!(!text.is_empty(), "missing translation for {english}");
                    assert_eq!(language.text(english), text, "conflicting translation for {english}");
                    assert_eq!(placeholders(english), placeholders(text), "lost template values in {english}");
                }
            }
        }
    }
    #[test]
    fn one_english_key_has_one_set_of_words_across_every_table() {
        let mut seen: std::collections::BTreeMap<&str, Entry> = std::collections::BTreeMap::new();
        let all = rows();
        assert_eq!(all.len(), tables().iter().map(|table| table.len()).sum::<usize>());
        for row in all {
            if let Some(first) = seen.insert(row.0, row) {
                assert_eq!(first, row, "two translations for {}", row.0);
            }
        }
        // A key whose words are its English in every language is still a
        // key, and one no table has is not.
        assert!(has_entry("{0} · {1}"));
        assert_eq!(Language::Japanese.text("{0} · {1}"), "{0} · {1}");
        assert!(has_entry("Not linked"));
        assert!(!has_entry("A sentence no table has"));
        assert!(!has_entry(""));
    }
    #[test]
    fn interpolation_preserves_host_text_and_unknown_keys() {
        for language in Language::ALL {
            assert_eq!(language.text("Untranslated user content"), "Untranslated user content");
            assert_eq!(language.format("{0} / {1}", &["Settings {1}", "Ready"]), "Settings {1} / Ready");
            assert_eq!(language.format_named("{name}: {reason}", &[("name", "Ready {reason}"), ("reason", "Settings")]), "Ready {reason}: Settings");
        }
    }
}
