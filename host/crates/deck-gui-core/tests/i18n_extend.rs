//! The app's own string tables, added to the four this crate ships with
//! `i18n::extend`. A test binary of its own: the tables are installed once
//! per process, and the crate's other tests check the built-in tables alone.
use deck_gui_core::i18n::{self, Entry, Language};

/// A page's table: one row of its own, and one that repeats a built-in key
/// with other words.
const PAGE: &[Entry] = &[
    (
        "A row only this page has",
        "只有此页面有的行",
        "只有此頁面有的列",
        "このページだけの行",
    ),
    ("Deck", "别的词", "別的詞", "別の言葉"),
];
const TABLES: &[&[Entry]] = &[PAGE];
const OTHER: &[&[Entry]] = &[&[("Another row", "另一行", "另一列", "別の行")]];

#[test]
fn the_apps_tables_are_found_after_the_built_in_ones_and_added_only_once() {
    let english = "A row only this page has";
    assert!(!i18n::has_entry(english));
    assert_eq!(Language::Japanese.text(english), english, "missing: the English");
    let before = i18n::rows().len();

    assert!(i18n::extend(TABLES));
    assert!(i18n::has_entry(english));
    assert_eq!(Language::SimplifiedChinese.text(english), "只有此页面有的行");
    assert_eq!(Language::TraditionalChinese.text(english), "只有此頁面有的列");
    assert_eq!(Language::Japanese.text(english), "このページだけの行");
    assert_eq!(
        Language::Japanese.format("{0} · {1}", &[english, "x"]),
        format!("{english} · x"),
        "a value is never looked up"
    );
    // A built-in key keeps its built-in words: the app's tables come after.
    assert_eq!(Language::SimplifiedChinese.text("Deck"), "设备");
    // Every row of every table, the app's last.
    let rows = i18n::rows();
    assert_eq!(rows.len(), before + PAGE.len());
    assert_eq!(rows[rows.len() - 2].0, english);

    // Once per process: the first tables stay.
    assert!(!i18n::extend(OTHER));
    assert!(!i18n::has_entry("Another row"));
    assert_eq!(Language::Japanese.text("Another row"), "Another row");
    assert!(i18n::has_entry(english));
    assert!(!i18n::has_entry("A sentence no table has"));
}
