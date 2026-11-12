//! The Deck page's words, each as `(English, 简体中文, 繁體中文, 日本語)`.
//! English is the key.
//!
//! The rows are the deck sheet's (`deck.tsv` of the string sheets), copied
//! as written and in its order. A word the page shares with another page
//! (Bluetooth, Waiting, Details, the plate titles, the old-helper lines, the
//! not-running row…) is in `crate::i18n::kit`, and one the built-in tables
//! already ship (Deck, Linked, Needs attention, Language, Start, Close…) is
//! not repeated: one key has one set of words wherever it is. Battery is the
//! deck's own 电池 / 電池 / バッテリー, not the board's 电量.
use deck_gui_core::i18n::Entry;

pub const STRINGS: &[Entry] = &[
    // The page's head, and the deck's own card: its rows and its caption.
    (
        "The device in your hand: its link, its battery, its settings.",
        "你手里的设备：连接、电量和各项设置。",
        "你手上的裝置：連線、電量和各項設定。",
        "お手元のデバイス: 接続、バッテリー、各種設定。",
    ),
    ("Your deck", "你的设备", "你的裝置", "お使いのデバイス"),
    (
        "Showing the {0} face",
        "正在显示“{0}”页面",
        "正在顯示「{0}」頁面",
        "「{0}」ページを表示中",
    ),
    ("Link", "连接", "連線", "接続"),
    ("Not answering", "无响应", "無回應", "応答なし"),
    ("Battery", "电池", "電池", "バッテリー"),
    (" · charging", " · 充电中", " · 充電中", " · 充電中"),
    ("Firmware", "固件", "韌體", "ファームウェア"),
    ("Case", "外壳", "外殼", "本体カラー"),
    // What needs attention, one card each, and what each card offers.
    (
        "The link reconnected {0} times in the last hour",
        "过去一小时内连接断开重连了 {0} 次",
        "過去一小時內連線中斷重連了 {0} 次",
        "この 1 時間で {0} 回再接続しました",
    ),
    (
        "Try the cable. If it goes on, send the support file.",
        "请先改用数据线。如果仍然出现，请发送支持文件。",
        "請先改用連接線。如果問題持續，請傳送支援檔案。",
        "ケーブル接続を試し、続く場合はサポートファイルを送信。",
    ),
    (
        "The deck is not answering",
        "设备无响应",
        "裝置無回應",
        "デバイスが応答しません",
    ),
    (
        "It is linked but silent. Move it closer or use the cable.",
        "设备已连接但没有响应。请靠近一些，或改用数据线。",
        "裝置已連線但沒有回應。請靠近一些，或改用連接線。",
        "接続は保たれています。近づけるか、ケーブルを使ってください。",
    ),
    (
        "The service needs attention",
        "服务需要关注",
        "服務需要注意",
        "サービスの確認が必要",
    ),
    (
        "Deck firmware {0} is available",
        "设备固件 {0} 现已可用",
        "裝置韌體 {0} 現已可用",
        "ファームウェア {0} が利用可能",
    ),
    (
        "Install it by cable from Service.",
        "请在“服务”中通过数据线安装。",
        "請在「服務」中透過連接線安裝。",
        "「サービス」から、ケーブル接続で\nインストールしてください。",
    ),
    // The deck's own settings, in its panels' names and order (the deck's
    // own words, read from its tables).
    (
        "Volume and speech",
        "音量与播报",
        "音量與播報",
        "音量と音声",
    ),
    ("Speaker", "扬声器", "揚聲器", "スピーカー"),
    ("Mic gain", "麦克风增益", "麥克風增益", "マイク感度"),
    ("Auto-announce", "自动播报", "自動播報", "自動読み上げ"),
    ("Alerts", "提醒", "提醒", "通知"),
    ("Chime", "提示音", "提示音", "通知音"),
    ("Auto-jump", "自动跳转", "自動跳轉", "自動切替"),
    ("Display", "显示", "顯示", "表示"),
    ("Brightness", "亮度", "亮度", "明るさ"),
    (
        "Idle home",
        "空闲返回首页",
        "閒置返回首頁",
        "待機時にホーム",
    ),
    ("Never", "从不", "永不", "なし"),
    ("30 s", "30 秒", "30 秒", "30 秒"),
    ("1 min", "1 分钟", "1 分鐘", "1 分"),
    ("2 min", "2 分钟", "2 分鐘", "2 分"),
    ("Text size", "文字大小", "文字大小", "文字サイズ"),
    ("Small", "小", "小", "小"),
    ("Medium", "中", "中", "中"),
    ("Large", "大", "大", "大"),
    (
        "The app and the deck change together.",
        "应用和设备会一起切换。",
        "應用程式和裝置會一起切換。",
        "アプリとデバイスが一緒に切り替わります。",
    ),
    // No deck linked: when it last was, and what the owner can still do.
    (
        "Last linked {0}",
        "上次连接：{0}",
        "上次連線：{0}",
        "最終接続: {0}",
    ),
    (
        "The background service is waiting for your deck",
        "后台服务正在等待设备连接",
        "背景服務正在等待你的裝置",
        "バックグラウンドサービスがデバイスの接続を待っています",
    ),
    (
        "If the screen is dark",
        "如果屏幕不亮",
        "如果螢幕不亮",
        "画面が消えているとき",
    ),
    (
        "Hold PTT, the long key on the left edge, for 2 seconds.",
        "按住 PTT（左侧的长条按键）2 秒。",
        "按住 PTT（左側邊緣的長條按鍵）2 秒。",
        "PTT（左側面の長いキー）を 2 秒間押し続けます。",
    ),
    (
        "Cable or Bluetooth",
        "数据线或蓝牙",
        "連接線或藍牙",
        "ケーブルまたは Bluetooth",
    ),
    (
        "Use the cable, or pair it again.",
        "使用数据线，或重新配对。",
        "使用連接線，或重新配對。",
        "ケーブルか再ペアリングで接続します。",
    ),
    (
        "Still yours to set",
        "仍可设置",
        "仍可設定",
        "今も設定できる項目",
    ),
    (
        "The deck follows when it links.",
        "设备连接后会跟着切换。",
        "裝置連線後會跟著切換。",
        "デバイスは接続時に同じ言語に切り替わります。",
    ),
    (
        "Volume, alerts and display settings come back when the deck links.",
        "设备连接后，音量、提醒和显示设置会重新出现。",
        "裝置連線後，音量、提醒和顯示設定會重新出現。",
        "音量、通知、表示の設定は、デバイスを接続するとまた変更できます。",
    ),
    // The service not answering (the titles and buttons for why are shared,
    // in `kit`: the frame's strip and the Service page say them too).
    (
        "The deck works only while it runs.",
        "设备仅在后台服务运行时工作。",
        "裝置僅在背景服務執行時運作。",
        "サービスの実行中のみ動作します。",
    ),
    (
        "Needs the background service.",
        "需要后台服务。",
        "需要背景服務。",
        "バックグラウンドサービスが必要です。",
    ),
    // The link's details: the drops of the last hour and why each happened.
    ("Link problem", "连接问题", "連線問題", "接続の問題"),
    ("read error", "读取错误", "讀取錯誤", "読み取りエラー"),
    ("write error", "写入错误", "寫入錯誤", "書き込みエラー"),
    ("write stalled", "写入停滞", "寫入停滯", "書き込みが停止"),
    (
        "unplugged or out of range",
        "已拔出或超出范围",
        "已拔除或超出範圍",
        "取り外しまたは圏外",
    ),
    ("stopped answering", "停止响应", "停止回應", "応答が停止"),
    (
        "No drops in the last hour.",
        "过去一小时内没有断开。",
        "過去一小時內沒有中斷。",
        "この 1 時間に切断はありません。",
    ),
];
