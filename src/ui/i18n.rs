//! Device-owned interface language. Only explicitly marked interface strings
//! pass through this catalogue; host documents and user names are never
//! translated by the font renderer. Numeric ids are persisted on the device.
mod common;
mod agents;
mod faces;
mod overlays;
mod settings;
#[path = "../language.rs"]
mod language;
pub use language::Language;

/// English source key, Simplified Chinese, Traditional Chinese, Japanese.
pub type Entry = (&'static str, &'static str, &'static str, &'static str);

impl Language {
    /// Explicit UI lookup; unknown source keys retain their English text.
    pub fn text(self, english: &str) -> &str {
        if self == Self::English { return english; }
        for table in tables() {
            if let Some(entry) = table.iter().find(|entry| entry.0 == english) {
                return match self {
                    Self::English => entry.0,
                    Self::SimplifiedChinese => entry.1,
                    Self::TraditionalChinese => entry.2,
                    Self::Japanese => entry.3,
                };
            }
        }
        english
    }
}

fn tables() -> [&'static [Entry]; 5] {
    [common::STRINGS, agents::STRINGS, faces::STRINGS, overlays::STRINGS, settings::STRINGS]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_ids_names_and_fallback_are_stable() {
        for (id, language) in Language::ALL.into_iter().enumerate() {
            assert_eq!(language as usize, id);
            assert_eq!(Language::from_id(id as u8), language);
            assert!(!language.name().is_empty() && !language.code().is_empty());
            assert_eq!(language.text("untranslated host content"), "untranslated host content");
        }
        assert_eq!(Language::from_id(255), Language::English);
    }

    #[test]
    fn every_catalogue_entry_has_all_four_languages_and_supported_glyphs() {
        for table in tables() {
            for entry in table {
                for text in [entry.0, entry.1, entry.2, entry.3] {
                    assert!(!text.is_empty(), "missing translation: {}", entry.0);
                    for character in text.chars().filter(|character| *character as u32 >= 0x2e80) {
                        assert!(crate::ui::font_cjk::source(character as u32).is_some(), "missing glyph {character} in {}", entry.0);
                    }
                }
                assert_eq!(Language::English.text(entry.0), entry.0);
                for (language, expected) in [(Language::SimplifiedChinese, entry.1), (Language::TraditionalChinese, entry.2), (Language::Japanese, entry.3)] {
                    assert_eq!(language.text(entry.0), expected, "conflicting translation: {}", entry.0);
                }
            }
        }
    }
}
