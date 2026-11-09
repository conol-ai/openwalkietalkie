use super::*;
use crate::ui::i18n::Language;

#[test]
fn language_and_tutorial_preferences_survive_native_restart_without_overwriting_each_other() {
    let _guard = lock(&CORE_LOCK);
    reset();
    let prefs = bond_record::Prefs { tutorial_done: true, language: Language::SimplifiedChinese as u8, language_revision: 1 };
    *lock(&UI_PREFS) = Some(prefs.encode());
    unsafe {
        owt_core_init();
        owt_core_load_ui_prefs();
        let core = &mut *core::ptr::addr_of_mut!(CORE);
        assert_eq!(core.ui.language, Language::SimplifiedChinese);
        let mut update = Update::new();
        update.language = Some(Language::Japanese);
        core.ui.language_revision = 2;
        emit(core, &mut update);
        assert_eq!(core.prefs, bond_record::Prefs { tutorial_done: true, language: Language::Japanese as u8, language_revision: 2 });
        assert_eq!(UI_PREFS_WRITES.load(Ordering::Relaxed), 1);
        emit(core, &mut update);
        assert_eq!(UI_PREFS_WRITES.load(Ordering::Relaxed), 1, "an unchanged selection does not write flash again");
        owt_core_init();
        owt_core_load_ui_prefs();
        let core = &mut *core::ptr::addr_of_mut!(CORE);
        assert_eq!(core.ui.language, Language::Japanese);
        let mut update = Update::new();
        update.tutorial_done = true;
        emit(core, &mut update);
        assert_eq!(core.prefs.language, Language::Japanese as u8);
    }
}

#[test]
fn native_preferences_fall_back_to_english_on_missing_or_corrupted_data() {
    let _guard = lock(&CORE_LOCK);
    reset();
    unsafe {
        owt_core_init();
        owt_core_load_ui_prefs();
        assert_eq!((*core::ptr::addr_of!(CORE)).ui.language, Language::English);
        *lock(&UI_PREFS) = Some([0xff; bond_record::SIZE]);
        owt_core_init();
        owt_core_load_ui_prefs();
        assert_eq!((*core::ptr::addr_of!(CORE)).ui.language, Language::English);
    }
}
