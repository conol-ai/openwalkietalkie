//! Words two or more pages show, the units of the shared formatters, the
//! face names and the plate titles, and the pages' names, each as
//! `(English, 简体中文, 繁體中文, 日本語)`. English is the key.
//!
//! The rows are the kit sheet's (`kit.tsv` of the string sheets), copied as
//! written: a row the built-in tables already ship (On, Off, Cancel, Done,
//! Deck, Settings…) is not here, because one key has one set of words
//! wherever it is. A string two pages show lives here and not in either
//! page's table, so a page's table holds only what that page says and no
//! key is in two tables.
use deck_gui_core::i18n::Entry;

pub const STRINGS: &[Entry] = &[
    // Shared words: states, buttons, the link.
    ("Paused", "已暂停", "已暫停", "一時停止"),
    ("Granted", "已授权", "已授權", "許可済み"),
    ("Installed", "已安装", "已安裝", "インストール済み"),
    ("Not set up", "未设置", "未設定", "未設定"),
    ("Needs repair", "需要修复", "需要修復", "修復が必要"),
    ("Not checked", "未检查", "未檢查", "未確認"),
    ("Details", "详情", "詳細資料", "詳細"),
    ("Continue", "继续", "繼續", "続ける"),
    ("Remove…", "移除…", "移除…", "削除…"),
    ("Delete…", "删除…", "刪除…", "削除…"),
    ("Bluetooth", "蓝牙", "藍牙", "Bluetooth"),
    ("Cable", "数据线", "連接線", "ケーブル"),
    ("Waiting", "等待中", "等待中", "待機中"),
    // `present::ago` and `present::day_and_time`.
    ("just now", "刚刚", "剛剛", "たった今"),
    ("{0} s ago", "{0} 秒前", "{0} 秒前", "{0} 秒前"),
    ("{0} min ago", "{0} 分钟前", "{0} 分鐘前", "{0} 分前"),
    ("{0} h ago", "{0} 小时前", "{0} 小時前", "{0} 時間前"),
    ("{0} d ago", "{0} 天前", "{0} 天前", "{0} 日前"),
    ("today, {0}", "今天 {0}", "今天 {0}", "今日 {0}"),
    ("yesterday, {0}", "昨天 {0}", "昨天 {0}", "昨日 {0}"),
    ("{0}, {1}", "{0} {1}", "{0} {1}", "{0} {1}"),
    // `present::size_text`, and a percentage.
    ("{0} KB", "{0} KB", "{0} KB", "{0} KB"),
    ("{0} MB", "{0} MB", "{0} MB", "{0} MB"),
    ("{0} GB", "{0} GB", "{0} GB", "{0} GB"),
    ("{0}%", "{0}%", "{0}%", "{0}%"),
    // The face names, and the titles the deck's face plates carry (the
    // deck's own words, read from its tables).
    ("Macropad", "快捷键", "快捷鍵", "マクロパッド"),
    ("System", "系统", "系統", "システム"),
    ("Timer", "计时器", "計時器", "タイマー"),
    ("Arcade", "游戏", "遊戲", "ゲーム"),
    ("AGENTS", "助手", "助理", "エージェント"),
    ("MACROPAD", "快捷键", "快捷鍵", "マクロパッド"),
    ("USAGE", "用量", "用量", "使用量"),
    ("SYSTEM", "系统", "系統", "システム"),
    ("TIMER", "计时器", "計時器", "タイマー"),
    ("ARCADE", "游戏", "遊戲", "ゲーム"),
    ("SETTINGS", "设置", "設定", "設定"),
    // The pages, as the sidebar and the window name them.
    ("Faces & Keys", "页面与按键", "頁面與按鍵", "ページとキー"),
    // Sentences more than one page says.
    ("On the deck", "在设备上", "在裝置上", "デバイスに表示"),
    ("White", "白色", "白色", "ホワイト"),
    ("Black", "黑色", "黑色", "ブラック"),
    (
        "Open Bluetooth Settings",
        "打开蓝牙设置",
        "開啟藍牙設定",
        "Bluetooth 設定を開く",
    ),
    (
        "Background service",
        "后台服务",
        "背景服務",
        "バックグラウンドサービス",
    ),
    (
        "Claude Code plugin",
        "Claude Code 插件",
        "Claude Code 外掛",
        "Claude Code プラグイン",
    ),
    (
        "The background service is not running",
        "后台服务未运行",
        "背景服務未執行",
        "バックグラウンドサービスが停止中",
    ),
    ("Open Log", "打开日志", "開啟記錄", "ログを開く"),
    // Why no service answers, and what to do about it: the Deck page, the
    // frame's strip and the Service page say these alike.
    (
        "The background service is not installed",
        "后台服务尚未安装",
        "背景服務尚未安裝",
        "バックグラウンドサービスが未インストール",
    ),
    ("Set Up…", "设置…", "設定…", "設定する…"),
    (
        "The background service is waiting for your approval",
        "后台服务正在等待你的批准",
        "背景服務正在等待你的核准",
        "バックグラウンドサービスが承認待ち",
    ),
    (
        "Open Service",
        "打开“服务”",
        "開啟「服務」",
        "「サービス」を開く",
    ),
    (
        "Send a Test Question",
        "发送测试提问",
        "傳送測試提問",
        "テスト質問を送信",
    ),
    (
        "Link the deck first.",
        "请先连接设备。",
        "請先連線裝置。",
        "先にデバイスを接続してください。",
    ),
    (
        "A test question is on the deck. Answer it there.",
        "测试提问已显示在设备上，请在设备上回答。",
        "測試提問已顯示在裝置上，請在裝置上回答。",
        "テスト質問をデバイスに表示しました。デバイスで回答してください。",
    ),
    (
        "The last test question arrived on the deck and was answered there after {0} s.",
        "上次的测试提问已送达设备，{0} 秒后在设备上得到回答。",
        "上次的測試提問已送達裝置，並在 {0} 秒後於裝置上獲得回答。",
        "前回のテスト質問はデバイスに届き、{0} 秒後にデバイスで回答されました。",
    ),
    (
        "The last test question was not answered on the deck.",
        "上次的测试提问未在设备上得到回答。",
        "上次的測試提問未在裝置上獲得回答。",
        "前回のテスト質問はデバイスで回答されませんでした。",
    ),
    (
        "The deck is not linked, so no test question was sent.",
        "设备未连接，因此没有发送测试提问。",
        "裝置未連線，因此沒有傳送測試提問。",
        "デバイスが未接続のため、テスト質問は送信していません。",
    ),
    (
        "1 Claude session uses an old helper",
        "1 个 Claude 会话在使用旧的辅助程序",
        "1 個 Claude 工作階段在使用舊的輔助程式",
        "1 件の Claude セッションが古いヘルパーを使用中",
    ),
    (
        "{0} Claude sessions use an old helper",
        "{0} 个 Claude 会话在使用旧的辅助程序",
        "{0} 個 Claude 工作階段在使用舊的輔助程式",
        "{0} 件の Claude セッションが古いヘルパーを使用中",
    ),
    (
        "Restart that session.",
        "请重启该会话。",
        "請重新啟動該工作階段。",
        "そのセッションを再起動してください。",
    ),
    (
        "Restart those sessions.",
        "请重启这些会话。",
        "請重新啟動這些工作階段。",
        "対象のセッションを再起動してください。",
    ),
    (
        "Codex is not running",
        "Codex 未运行",
        "Codex 未執行",
        "Codex は停止中",
    ),
    ("Not running", "未运行", "未執行", "停止中"),
    ("Copy Command", "复制命令", "複製指令", "コマンドをコピー"),
    (
        "One more step: run this command in a terminal.",
        "还差一步：请在终端中运行此命令。",
        "還差一步：請在終端機中執行此指令。",
        "あと 1 ステップです。ターミナルでこのコマンドを実行してください。",
    ),
    (
        "This needs a newer background service.",
        "此功能需要较新版本的后台服务。",
        "此功能需要較新版本的背景服務。",
        "この機能には新しいバージョンのバックグラウンドサービスが必要です。",
    ),
];
