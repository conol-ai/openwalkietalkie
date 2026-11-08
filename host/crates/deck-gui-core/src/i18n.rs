//! Desktop interface copy. Translation is explicit: host/user content never
//! passes through an automatic renderer or substring replacement.
#[path = "../../../../src/language.rs"]
mod language;
pub use language::Language;
mod app;
mod auxiliary;
mod common;
mod platform;

/// English source template, Simplified Chinese, Traditional Chinese, Japanese.
pub type Entry = (&'static str, &'static str, &'static str, &'static str);

fn tables() -> [&'static [Entry]; 4] {
    [common::STRINGS, app::STRINGS, auxiliary::STRINGS, platform::STRINGS]
}

impl Language {
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

    pub fn format(self, english: &str, values: &[&str]) -> String {
        render(self.text(english), |key| key.parse::<usize>().ok().and_then(|index| values.get(index).copied()))
    }

    pub fn format_named(self, english: &str, values: &[(&str, &str)]) -> String {
        render(self.text(english), |key| values.iter().find(|(name, _)| *name == key).map(|(_, value)| *value))
    }
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
    fn interpolation_preserves_host_text_and_unknown_keys() {
        for language in Language::ALL {
            assert_eq!(language.text("Untranslated user content"), "Untranslated user content");
            assert_eq!(language.format("{0} / {1}", &["Settings {1}", "Ready"]), "Settings {1} / Ready");
            assert_eq!(language.format_named("{name}: {reason}", &[("name", "Ready {reason}"), ("reason", "Settings")]), "Ready {reason}: Settings");
        }
    }
}
