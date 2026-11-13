//! The frame's words (the sidebar, the strips, the status item and the
//! app's menus), each as `(English, 简体中文, 繁體中文, 日本語)`.
//! English is the key.
//!
//! The rows are the frame sheet's own (`frame.tsv` of the string sheets),
//! copied as written, then the menu bar's. The rest of what the frame says
//! is in a table that ships already or in [`super::kit`]: the pages' names
//! (the View menu's items too), "Bluetooth" and "Cable", the figure's
//! `{0}%`, the strip's words, "Close", and the status item's labels and
//! status lines, which `deck_gui_core` speaks itself.
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
    // The menu bar's menus and items, in the words macOS itself uses for
    // them in each language (`app::menus`).
    ("Edit", "编辑", "編輯", "編集"),
    ("Undo", "撤销", "還原", "取り消す"),
    ("Redo", "重做", "重做", "やり直す"),
    ("Cut", "剪切", "剪下", "カット"),
    ("Copy", "拷贝", "拷貝", "コピー"),
    ("Paste", "粘贴", "貼上", "ペースト"),
    ("Select All", "全选", "全選", "すべてを選択"),
    ("View", "显示", "顯示方式", "表示"),
    ("Window", "窗口", "視窗", "ウインドウ"),
    ("Minimize", "最小化", "縮到最小", "しまう"),
    ("Zoom", "缩放", "縮放", "拡大/縮小"),
];
