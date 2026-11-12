//! The frame's words (the sidebar, the strips and the status item), each as
//! `(English, 简体中文, 繁體中文, 日本語)`.
//! English is the key.
//!
//! The rows are the frame sheet's own (`frame.tsv` of the string sheets),
//! copied as written. The rest of what the frame says is in a table that
//! ships already or in [`super::kit`]: the pages' names, "Bluetooth" and
//! "Cable", the figure's `{0}%`, the strip's words, and the menu's labels
//! and status lines, which `deck_gui_core` speaks itself.
use deck_gui_core::i18n::Entry;

pub const STRINGS: &[Entry] = &[
    // The sidebar's foot card while the deck is linked ("Not linked" ships
    // with the menu's status lines).
    ("Deck linked", "设备已连接", "裝置已連線", "デバイス接続済み"),
    // The status item's third icon, as the MenuBar board names it.
    ("Needs care", "需要关注", "需要注意", "確認が必要"),
    // The status item's tooltip, one for each icon: what its shape means.
    (
        "OpenWalkieTalkie status item, the deck is linked",
        "OpenWalkieTalkie 菜单栏图标，设备已连接",
        "OpenWalkieTalkie 選單列圖像，裝置已連線",
        "OpenWalkieTalkie メニューバー項目、デバイス接続済み",
    ),
    (
        "OpenWalkieTalkie status item, the deck is not linked",
        "OpenWalkieTalkie 菜单栏图标，设备未连接",
        "OpenWalkieTalkie 選單列圖像，裝置未連線",
        "OpenWalkieTalkie メニューバー項目、デバイス未接続",
    ),
    (
        "OpenWalkieTalkie status item, the deck needs care",
        "OpenWalkieTalkie 菜单栏图标，设备需要关注",
        "OpenWalkieTalkie 選單列圖像，裝置需要注意",
        "OpenWalkieTalkie メニューバー項目、デバイスの確認が必要",
    ),
];
