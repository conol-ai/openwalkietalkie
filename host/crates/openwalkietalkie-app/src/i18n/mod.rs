//! The app's own string tables: one per page and one of shared words, each
//! a list of `deck_gui_core`'s four-column
//! [`Entry`](deck_gui_core::i18n::Entry) — English, which is the key, then
//! Simplified Chinese, Traditional Chinese and Japanese.
//!
//! They are handed to [`deck_gui_core::i18n::extend`] once, by [`install`],
//! and scanned after the four built-in tables, so a string the window says
//! is looked up by the same `tr`/`fmt` whichever table holds it. A page's
//! table holds the words that page alone says; a row two pages show is in
//! [`kit`], and a key is in one table only, so it has one set of words.
//!
//! **A string with no row is shown in English in every language, and
//! nothing fails.** So each page's test reads its own sources with
//! [`missing_rows`], which names every literal handed to a lookup that no
//! table has, and pins how many [`literals`] it found, so a lookup the
//! scanner stopped seeing cannot pass as a page with nothing to translate.
use deck_gui_core::i18n::Entry;

pub mod deck;
pub mod faces;
pub mod firstrun;
pub mod frame;
pub mod integrations;
pub mod kit;
pub mod service;
pub mod voice;

/// Every table of the app, shared words first.
pub const TABLES: &[&[Entry]] = &[
    kit::STRINGS,
    frame::STRINGS,
    deck::STRINGS,
    faces::STRINGS,
    integrations::STRINGS,
    voice::STRINGS,
    service::STRINGS,
    firstrun::STRINGS,
];

/// Hand [`TABLES`] to `deck_gui_core`, once per process. `false` when
/// tables were already installed (they stay as they were).
pub fn install() -> bool {
    deck_gui_core::i18n::extend(TABLES)
}

/// The lookups whose first argument is a key of the tables: the shell's
/// `tr` and `fmt`, called on anything.
const LOOKUPS: [&str; 2] = ["tr", "fmt"];

/// `Language`'s own lookups, which count only as a method: after a `.`.
const METHODS: [&str; 2] = ["text", "format"];

/// Every string literal `source` hands to a lookup — `tr(`, `fmt(`,
/// `.text(` or `.format(` — as the program sees it (its escapes undone), in
/// the order they appear, repeats included.
///
/// It reads the source the way the compiler does as far as that matters
/// here: comments and every other literal are skipped, so a call mentioned
/// in a comment or a string is not one, and white space and line breaks
/// may stand between the name, the parenthesis and the literal, so a call
/// `rustfmt` laid out over three lines is still found. A call whose first
/// argument is not a literal (a variable, a `match`) has nothing it can
/// check and is not counted.
pub fn literals(source: &str) -> Vec<String> {
    let chars: Vec<char> = source.chars().collect();
    let mut found = Vec::new();
    // Whether the token before this one was a `.`.
    let mut dotted = false;
    let mut i = skip_space(&chars, 0);
    while let Some(&c) = chars.get(i) {
        let mut dot = false;
        if let Some((_, end)) = string_literal(&chars, i) {
            i = end;
        } else if c == '\'' {
            i = quote_end(&chars, i);
        } else if c == '_' || c.is_alphabetic() {
            let start = i;
            while chars
                .get(i)
                .is_some_and(|&c| c == '_' || c.is_alphanumeric())
            {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let lookup = LOOKUPS.contains(&word.as_str())
                || (dotted && METHODS.contains(&word.as_str()));
            let open = skip_space(&chars, i);
            if lookup && chars.get(open) == Some(&'(') {
                if let Some((text, _)) = string_literal(&chars, skip_space(&chars, open + 1)) {
                    found.push(text);
                }
            }
        } else {
            dot = c == '.';
            i += 1;
        }
        dotted = dot;
        i = skip_space(&chars, i);
    }
    found
}

/// The [`literals`] of `source` that no table has a row for, each named
/// once, in the order they first appear. Every table this build compiles is
/// asked, the app's included ([`install`]).
pub fn missing_rows(source: &str) -> Vec<String> {
    install();
    let mut missing: Vec<String> = Vec::new();
    for literal in literals(source) {
        if !deck_gui_core::i18n::has_entry(&literal) && !missing.contains(&literal) {
            missing.push(literal);
        }
    }
    missing
}

/// Past the white space and the comments from `i` on.
fn skip_space(chars: &[char], mut i: usize) -> usize {
    loop {
        match (chars.get(i), chars.get(i + 1)) {
            (Some(c), _) if c.is_whitespace() => i += 1,
            (Some('/'), Some('/')) => {
                while chars.get(i).is_some_and(|&c| c != '\n') {
                    i += 1;
                }
            }
            (Some('/'), Some('*')) => i = block_comment_end(chars, i),
            _ => return i,
        }
    }
}

/// Past the block comment that opens at `i`. Block comments nest.
fn block_comment_end(chars: &[char], mut i: usize) -> usize {
    let mut depth = 0usize;
    while let Some(&c) = chars.get(i) {
        match (c, chars.get(i + 1)) {
            ('/', Some('*')) => {
                depth += 1;
                i += 2;
            }
            ('*', Some('/')) => {
                depth = depth.saturating_sub(1);
                i += 2;
                if depth == 0 {
                    return i;
                }
            }
            _ => i += 1,
        }
    }
    i
}

/// The string literal that starts at `i` — `"…"`, `r"…"`, `r#"…"#` or a
/// byte form of one — as its value, and where it ends. `None` when no string
/// literal starts there.
fn string_literal(chars: &[char], i: usize) -> Option<(String, usize)> {
    let mut start = i;
    if chars.get(start) == Some(&'b') {
        start += 1;
    }
    if chars.get(start) == Some(&'r') {
        let mut quote = start + 1;
        while chars.get(quote) == Some(&'#') {
            quote += 1;
        }
        if chars.get(quote) != Some(&'"') {
            return None;
        }
        let hashes = quote - start - 1;
        let body = quote + 1;
        let mut end = body;
        while end < chars.len() {
            if chars[end] == '"' && (1..=hashes).all(|n| chars.get(end + n) == Some(&'#')) {
                return Some((chars[body..end].iter().collect(), end + 1 + hashes));
            }
            end += 1;
        }
        return Some((chars[body..].iter().collect(), chars.len()));
    }
    if chars.get(start) != Some(&'"') {
        return None;
    }
    let mut text = String::new();
    let mut k = start + 1;
    while let Some(&c) = chars.get(k) {
        match c {
            '"' => return Some((text, k + 1)),
            '\\' => k = unescape(chars, k + 1, &mut text),
            c => {
                text.push(c);
                k += 1;
            }
        }
    }
    Some((text, k))
}

/// The escape whose letter is at `i` (just after its `\`): push what it
/// stands for, and answer where the string goes on.
fn unescape(chars: &[char], i: usize, text: &mut String) -> usize {
    let code = |digits: String| u32::from_str_radix(&digits, 16).ok().and_then(char::from_u32);
    match chars.get(i) {
        Some('n') => text.push('\n'),
        Some('t') => text.push('\t'),
        Some('r') => text.push('\r'),
        Some('0') => text.push('\0'),
        Some('x') => {
            text.extend(code(chars.iter().skip(i + 1).take(2).collect()));
            return i + 3;
        }
        // `\u{2026}`
        Some('u') => {
            let close = (i..chars.len())
                .find(|&k| chars[k] == '}')
                .unwrap_or(chars.len());
            let digits = chars
                .get(i + 2..close)
                .map(|digits| digits.iter().filter(|&&c| c != '_').collect())
                .unwrap_or_default();
            text.extend(code(digits));
            return close + 1;
        }
        // A line continuation: the break and the indent after it are not
        // part of the string.
        Some(c) if c.is_whitespace() => {
            let mut k = i;
            while chars.get(k).is_some_and(|c| c.is_whitespace()) {
                k += 1;
            }
            return k;
        }
        // `\\`, `\"`, `\'`.
        Some(&c) => text.push(c),
        None => return i,
    }
    i + 1
}

/// Past the `'` at `i`: a character literal (`'"'`, `'\''`, `'\u{2026}'`)
/// whole, so the quote inside it is not taken for a string, or the one
/// character of a lifetime or a label.
fn quote_end(chars: &[char], i: usize) -> usize {
    match (chars.get(i + 1), chars.get(i + 2)) {
        (Some('\\'), _) => {
            // The escaped character, then up to the closing quote.
            let mut k = i + 3;
            while chars.get(k).is_some_and(|&c| c != '\'') {
                k += 1;
            }
            k + 1
        }
        (Some(_), Some('\'')) => i + 3,
        _ => i + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    /// A page's source as the scanner meets it: lookups laid out every way
    /// `rustfmt` and a hand lay them out, and every near miss.
    const SOURCE: &str = r##"
        // self.tr("In a line comment")
        /* self.tr("In a /* nested */ block comment") */
        /// [`Shell::tr`]("In a doc comment")
        let a = self.tr("Deck");
        let b = self.fmt(
            "{0} · {1}",
            &[&left, &right],
        );
        let c = language
            .text(  "Not linked"  );
        let d = language.format("A sentence no table has: {0}", &[&n]);
        let e = self.tr(english);
        let f = as_str("Not a lookup");
        let g = mistr("Not one either");
        let h = self.tr("Re-reading the service's state\u{2026}");
        let i = self.tr(r#"Raw {0}"#);
        let j = ('"', '\'', b'"', '\u{22}');
        let k: &'static str = self.tr("Done");
        let l = format!("{0} failed: {1}", what, why);
        let m = text("Not a method call");
        let n = self.tr("A sentence no table has: {0}");
        let o = "self.tr(\"In a string\")";
        let p = self.tr(
            // a comment before the literal
            "Cancel",
        );
        let q = self.tr("Line one \
                         line two");
        let r = ui::text_color(self.tr("today, {0}"));
    "##;

    #[test]
    fn every_literal_handed_to_a_lookup_is_found_however_the_call_is_laid_out() {
        assert_eq!(
            literals(SOURCE),
            vec![
                "Deck",
                "{0} · {1}",
                "Not linked",
                "A sentence no table has: {0}",
                "Re-reading the service's state\u{2026}",
                "Raw {0}",
                "Done",
                "A sentence no table has: {0}",
                "Cancel",
                "Line one line two",
                "today, {0}",
            ]
        );
        assert!(literals("").is_empty());
        // A source that ends inside a literal or a comment is read to its
        // end, not past it.
        assert_eq!(literals("self.tr(\"Deck"), vec!["Deck"]);
        assert!(literals("self.tr(/* never closed").is_empty());
    }

    #[test]
    fn only_a_literal_no_table_has_is_missing_and_each_is_named_once() {
        assert_eq!(
            missing_rows(SOURCE),
            vec![
                "A sentence no table has: {0}",
                "Raw {0}",
                "Line one line two",
            ]
        );
        // The app's own tables are asked, the formatters' words among them.
        assert!(missing_rows("x.format(\"{0} min ago\", &[&n])").is_empty());
    }

    /// The `{n}` placeholders a string carries.
    fn placeholders(text: &str) -> BTreeSet<&str> {
        let mut found = BTreeSet::new();
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
    fn every_row_has_four_translations_and_one_key_has_one_set_of_words_everywhere() {
        for table in TABLES {
            for &(english, hans, hant, japanese) in *table {
                for text in [english, hans, hant, japanese] {
                    assert!(!text.is_empty(), "missing translation for {english}");
                    assert_eq!(
                        placeholders(english),
                        placeholders(text),
                        "lost template values in {english}"
                    );
                }
            }
        }
        install();
        let all = deck_gui_core::i18n::rows();
        let app = TABLES.iter().map(|table| table.len()).sum::<usize>();
        assert!(all.len() >= app, "the app's tables are scanned");
        // The built-in tables and the app's together: a key that two of
        // them translate differently would read one way or the other
        // depending on which table a lookup reached first.
        let mut seen: BTreeMap<&str, Entry> = BTreeMap::new();
        for row in all {
            if let Some(first) = seen.insert(row.0, row) {
                assert_eq!(first, row, "two translations for {}", row.0);
            }
        }
    }
}
