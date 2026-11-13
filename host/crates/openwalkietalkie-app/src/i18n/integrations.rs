//! The Integrations page's words, each as
//! `(English, 简体中文, 繁體中文, 日本語)`. English is the key.
//!
//! The rows are the integrations sheet's (`integrations.tsv` of the string
//! sheets), copied as written and in its order, and the ones this page added
//! since: the installer's `outdated` explained with the button the page
//! has, the count's hint, the sessions no row counts, a connected app whose
//! chats are heard on this Mac, Codex hooks left from an older setup, and
//! the Ghostty-only hand-off a 0.13 service reports ("Focuses its tab"
//! promised every terminal), beside the app hand-off of later ones.
//! A word the page shares with another page (Paused, Not set up, Needs
//! repair, the test question's lines, On the deck, the ages, Copy
//! Command…) is in `crate::i18n::kit`, and one the built-in tables already
//! ship (Integrations, On, Off, Connected, Repair, Source, Last event, the
//! plan dialog's words…) is not repeated: one key has one set of words
//! wherever it is.
//!
//! One row is the Local API page's, which this table holds until the
//! Service page's own does: **Advanced**, over the token scopes that read
//! or steer a session.
use deck_gui_core::i18n::Entry;

pub const STRINGS: &[Entry] = &[
    // The page's head, and the line under it.
    (
        "What puts sessions on the deck, and whether each one is arriving.",
        "哪些来源会把会话送到设备上，以及它们是否正常送达。",
        "哪些來源會把工作階段送到裝置上，以及它們是否正常送達。",
        "デバイスにセッションを届ける接続元と、それぞれから届いているかどうか。",
    ),
    ("Pause integrations", "暂停集成", "暫停整合", "連携を一時停止"),
    (
        "This window shows counts and times, never a session.",
        "此窗口只显示数量和时间，从不显示会话内容。",
        "此視窗只顯示數量和時間，從不顯示工作階段內容。",
        "件数と時刻のみ表示し、セッション内容は表示しません。",
    ),
    (
        "Paused. Nothing new reaches the deck; questions stay in each tool's own window.",
        "已暂停。新的内容不会送到设备；提问会留在各工具自己的窗口中。",
        "已暫停。新的內容不會送到裝置；提問會留在各工具自己的視窗中。",
        "一時停止中です。新しい内容はデバイスに届かず、質問は各ツールのウィンドウに残ります。",
    ),
    // The table: its head, then each source's words.
    ("Agent tools", "助手工具", "助理工具", "エージェントツール"),
    ("Hand-off", "转到电脑", "轉至電腦", "引き継ぎ"),
    ("State", "状态", "狀態", "状態"),
    (
        "Plugin: {0} hooks and\nthe Channels helper.",
        "插件：{0} 个钩子和\nChannels 辅助程序。",
        "外掛：{0} 個掛鉤和\nChannels 輔助程式。",
        "プラグイン: フック {0} 個と\nChannels ヘルパー。",
    ),
    (
        "Plugin: {0} hooks.",
        "插件：{0} 个钩子。",
        "外掛：{0} 個掛鉤。",
        "プラグイン: フック {0} 個。",
    ),
    ("1 session", "1 个会话", "1 個工作階段", "1 セッション"),
    ("{0} sessions", "{0} 个会话", "{0} 個工作階段", "{0} セッション"),
    ("1 thread", "1 个对话", "1 個對話", "1 スレッド"),
    ("{0} threads", "{0} 个对话", "{0} 個對話", "{0} スレッド"),
    ("1 chat", "1 个聊天", "1 個聊天", "1 チャット"),
    ("{0} chats", "{0} 个聊天", "{0} 個聊天", "{0} チャット"),
    // The count's hint, around one of the counts above: every session a
    // source has, when more than hold a place on the deck.
    ("{0} in all", "共 {0}", "共 {0}", "合計 {0}"),
    // The hand-off column is 88 px: each translation breaks before the
    // Latin name, where its words part.
    (
        "Focuses its Ghostty tab",
        "切换到其\nGhostty 标签页",
        "切至其\nGhostty 分頁",
        "Ghostty の\nタブを前面へ",
    ),
    ("Raises its app", "切换到其应用", "切至其應用程式", "アプリを前面へ"),
    ("Not yet", "暂不支持", "尚未支援", "未対応"),
    ("By itself", "自行处理", "自行處理", "アプリ側で対応"),
    ("Set Up", "设置", "設定", "設定する"),
    // What **Repair**'s tooltip says of an out-of-date plugin
    // (`present::status_help`): the shipped sentence named an **Update**
    // button this page does not have; Repair is the button, in its shipped
    // words (修复 / 修復 / 修復).
    (
        "what is on disk differs from this build, Claude has not registered the plugin yet, or settings.json still carries the pre-plugin hooks; Repair rewrites or migrates it.",
        "磁盘内容与此版本不同、Claude 尚未注册插件，或 settings.json 仍有插件化之前的钩子；“修复”会重写或迁移它们。",
        "磁碟內容與此版本不同、Claude 尚未註冊外掛，或 settings.json 仍有外掛化之前的掛鉤；「修復」會重寫或移轉它們。",
        "ディスク上の内容がこのビルドと異なる、Claude にプラグインが未登録、または settings.json に従来のフックが残っています。「修復」で書き直すか移行します。",
    ),
    (
        "Voice orders arrive as your own message, not as another session's.",
        "语音指令会作为你自己的消息送达，而不是来自其他会话。",
        "語音指令會以你自己的訊息送達，而不是來自其他工作階段。",
        "音声の指示は、別セッションからではなく自分のメッセージとして届きます。",
    ),
    (
        "1 session started before the last update still uses the old helper.",
        "有 1 个会话在上次更新前启动，仍在使用旧的辅助程序。",
        "有 1 個工作階段在上次更新前啟動，仍在使用舊的輔助程式。",
        "更新前に開始した 1 セッションが、まだ古いヘルパーを使っています。",
    ),
    (
        "{0} sessions started before the last update still use the old helper.",
        "有 {0} 个会话在上次更新前启动，仍在使用旧的辅助程序。",
        "有 {0} 個工作階段在上次更新前啟動，仍在使用舊的輔助程式。",
        "更新前に開始した {0} セッションが、まだ古いヘルパーを使っています。",
    ),
    ("Restart 1 session", "重启 1 个会话", "重啟 1 個工作階段", "1 件を再起動"),
    ("Restart {0} sessions", "重启 {0} 个会话", "重啟 {0} 個工作階段", "{0} 件を再起動"),
    ("No session connected", "未连接会话", "未連線工作階段", "セッションなし"),
    // How a Claude session joins the Channels bridge.
    ("How to Connect…", "如何连接…", "如何連線…", "接続方法…"),
    (
        "Connect a Claude session",
        "连接 Claude 会话",
        "連線 Claude 工作階段",
        "Claude セッションを接続",
    ),
    (
        "In your project's terminal, start Claude Code with this command:",
        "在项目的终端中，用以下命令启动 Claude Code：",
        "在專案的終端機中，用以下指令啟動 Claude Code：",
        "プロジェクトのターミナルで、次のコマンドで Claude Code を起動します。",
    ),
    (
        "If Claude asks for a confirmation, accept it.",
        "如果 Claude 请求确认，请接受。",
        "如果 Claude 要求確認，請接受。",
        "Claude から確認を求められたら、承認してください。",
    ),
    (
        "Through Codex itself.\nNothing is installed.",
        "通过 Codex 自身连接。\n未安装任何内容。",
        "透過 Codex 本身連線。\n未安裝任何內容。",
        "Codex 自体を通じて接続。\nインストールなし。",
    ),
    // The same row while hooks of an older Codex setup are still in Codex's
    // own file: they reach the deck (installed as written), or are there in
    // some other state. The name column is 154 px, so each translation
    // breaks its own lines at 12 characters or fewer; the page itself is
    // named in quotes, as the glossary names a part of the app.
    (
        "Through Codex itself.\nLeftover hooks also reach the deck: remove them in Footprint.",
        "通过 Codex 自身连接。\n残留的钩子也会送到设备上，\n可在“安装内容”中移除。",
        "透過 Codex 本身連線。\n殘留的掛鉤也會送到裝置上，\n可在「安裝內容」中移除。",
        "Codex 自体を通じて接続。\n残っているフックからも\n届きます。削除は\n「インストール内容」で。",
    ),
    (
        "Through Codex itself.\nLeftover hooks are still on this Mac: remove them in Footprint.",
        "通过 Codex 自身连接。\n仍有残留的钩子，\n可在“安装内容”中移除。",
        "透過 Codex 本身連線。\n仍有殘留的掛鉤，\n可在「安裝內容」中移除。",
        "Codex 自体を通じて接続。\n残っているフックは\n「インストール内容」で\n削除できます。",
    ),
    (
        "Open “Codex with OpenWalkieTalkie” instead of Codex.\nIt is in the Applications folder of your home folder.",
        "请打开“Codex with OpenWalkieTalkie”，而不是 Codex。\n它位于个人文件夹的“应用程序”文件夹中。",
        "請開啟「Codex with OpenWalkieTalkie」，而不是 Codex。\n它位於「個人專屬」資料夾的「應用程式」資料夾中。",
        "Codex の代わりに「Codex with OpenWalkieTalkie」を開いてください。\nホームフォルダの「アプリケーション」フォルダにあります。",
    ),
    // The apps that connect by themselves.
    ("Connected apps", "已连接的应用", "已連線的應用程式", "接続アプリ"),
    (
        "Sessions. Voice for its\nchats: {0}'s cloud.",
        "会话。聊天语音\n由 {0} 云端识别。",
        "工作階段。聊天語音\n由 {0} 雲端辨識。",
        "セッション。チャットの\n音声認識: {0} クラウド。",
    ),
    // The same app with the Voice page's Conol switch off: the presses meant
    // for its chats are recognised on this Mac (the words the Voice page's
    // sentences use for "on this Mac", and those of the voice line above).
    (
        "Sessions. Its chats are\nheard on this Mac.",
        "会话。聊天语音\n在此 Mac 上识别。",
        "工作階段。聊天語音\n在此 Mac 上辨識。",
        "セッション。チャットの\n音声認識: この Mac 上。",
    ),
    (
        "Sessions, the Omarchy\nface and its keys.",
        "会话、Omarchy 页面\n及其按键。",
        "工作階段、Omarchy 頁面\n及其按鍵。",
        "セッション、Omarchy の\nページとキー。",
    ),
    ("Sessions.", "会话。", "工作階段。", "セッション。"),
    (
        "Other apps appear here when they connect. Each gets its own switch.",
        "其他应用连接后会显示在这里，并各有一个开关。",
        "其他應用程式連線後會顯示在這裡，並各有一個開關。",
        "ほかのアプリも接続するとここに表示され、それぞれにスイッチが付きます。",
    ),
    // Under the Agent tools card: sessions of a Gemini or an MCP adapter,
    // which no row counts.
    (
        "1 other session, from MCP or Gemini.",
        "另有 1 个会话来自 MCP 或 Gemini。",
        "另有 1 個工作階段來自 MCP 或 Gemini。",
        "ほかに MCP または Gemini のセッションが 1 件あります。",
    ),
    (
        "{0} other sessions, from MCP or Gemini.",
        "另有 {0} 个会话来自 MCP 或 Gemini。",
        "另有 {0} 個工作階段來自 MCP 或 Gemini。",
        "ほかに MCP または Gemini のセッションが {0} 件あります。",
    ),
    // The Local API page's (Service), held here until the Service page's
    // table has it: the disclosure over `operate`, `observe:content` and
    // `observe:raw`, in the words macOS gives an Advanced tab of settings.
    ("Advanced", "高级", "進階", "詳細"),
];
