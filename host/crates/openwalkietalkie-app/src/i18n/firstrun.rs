//! First run's words, each as `(English, 简体中文, 繁體中文, 日本語)`.
//! English is the key.
//!
//! The rows are the first-run sheet's (`firstrun.tsv` of the string sheets),
//! copied as written and in its order, and one this page added: `Deck
//! found`, for a deck found on a bus the probe cannot name (or by a service
//! too old to say), worded as the sheet's two found lines begin. A word the
//! page shares with another page (Continue, Open Bluetooth Settings, the
//! two rows' titles, the case colours, the plate's title, the test
//! question's lines, Copy Command…) is in `crate::i18n::kit`, and one the
//! built-in tables already ship (Voice, Integrations, Copied) is not
//! repeated: one key has one set of words wherever it is. The plugin row
//! says what a new install adds, the hooks alone (amendment O4).
use deck_gui_core::i18n::Entry;

pub const STRINGS: &[Entry] = &[
    // The stepper's first two steps (the other two are page names).
    ("Connect", "连接", "連接", "接続"),
    ("What this adds", "将添加的内容", "將新增的內容", "追加するもの"),
    // Step 1: looking for the deck.
    ("Looking for your deck", "正在寻找你的设备", "正在尋找你的裝置", "デバイスを探しています"),
    ("Connect your deck", "连接你的设备", "連接你的裝置", "デバイスを接続"),
    (
        "Plug it in with the USB-C cable. This window finds it by itself.",
        "用 USB-C 数据线接上设备，此窗口会自动找到它。",
        "用 USB-C 連接線接上裝置，此視窗會自動找到它。",
        "USB-C ケーブルでつなぐと、このウィンドウが自動で見つけます。",
    ),
    (
        "If macOS asks to allow the accessory, click Allow.",
        "如果 macOS 询问是否允许配件连接，请点击“允许”。",
        "如果 macOS 詢問是否允許配件連接，請按一下「允許」。",
        "macOS がアクセサリの接続許可を求めたら、「許可」をクリックします。",
    ),
    (
        "If the screen is dark, hold PTT, the long left-edge key, for 2 seconds.",
        "如果屏幕不亮，请按住 PTT（左侧的长条按键）2 秒。",
        "如果螢幕不亮，請按住 PTT（左側邊緣的長條按鍵）2 秒。",
        "画面が消えているときは、PTT（左側面の長いキー）を 2 秒間押し続けます。",
    ),
    (
        "It appears as OpenWalkieTalkie.",
        "它在列表中显示为 OpenWalkieTalkie。",
        "它在列表中顯示為 OpenWalkieTalkie。",
        "一覧には OpenWalkieTalkie と表示されます。",
    ),
    (
        "Nothing is installed yet. When your deck is found, the next step\nshows what this app adds and waits for you to agree.",
        "目前还没有安装任何内容。\n找到设备后，下一步会列出此应用将添加的内容，并等待你同意。",
        "目前尚未安裝任何內容。\n找到裝置後，下一步會列出此應用程式將新增的內容，並等待你同意。",
        "まだ何もインストールしていません。デバイスが見つかると、\n次のステップで追加するものを示し、同意を待ちます。",
    ),
    ("Join the waitlist", "加入候补名单", "加入候補名單", "ウェイトリストに登録"),
    ("Build your own", "自己动手制作", "自己動手製作", "自作する"),
    ("Try it in the browser", "在浏览器中试用", "在瀏覽器中試用", "ブラウザーで試す"),
    (
        "This copy cannot look for a deck",
        "此应用副本无法查找设备",
        "此應用程式副本無法尋找裝置",
        "このアプリのコピーではデバイスを探せません",
    ),
    // Step 2: what this app adds, and how adding it went.
    (
        "Deck found, linked by cable",
        "已找到设备，通过数据线连接",
        "已找到裝置，透過連接線連線",
        "デバイスが見つかりました（ケーブル接続）",
    ),
    (
        "Deck found, linked over Bluetooth",
        "已找到设备，通过蓝牙连接",
        "已找到裝置，透過藍牙連線",
        "デバイスが見つかりました（Bluetooth 接続）",
    ),
    (
        "What this app adds",
        "此应用将添加的内容",
        "此應用程式將新增的內容",
        "このアプリが追加するもの",
    ),
    (
        "Nothing has been added yet. These two keep the deck working\nwhen this window is closed.",
        "目前还没有添加任何内容。\n下面两项可让设备在此窗口关闭后继续工作。",
        "目前尚未新增任何內容。\n下面兩項可讓裝置在此視窗關閉後繼續運作。",
        "まだ何も追加していません。\n次の 2 つは、このウィンドウを閉じてもデバイスを使えるようにします。",
    ),
    (
        "Keeps the deck linked. Starts at login.",
        "保持设备连接，并在登录时启动。",
        "保持裝置連線，並在登入時啟動。",
        "デバイスとの接続を保ちます。ログイン時に起動します。",
    ),
    (
        "The Claude Code hooks, in this app's own folder.",
        "Claude Code 钩子，放在此应用自己的文件夹中。",
        "Claude Code 掛鉤，放在此應用程式自己的資料夾中。",
        "Claude Code のフック。このアプリ専用フォルダに保存。",
    ),
    (
        "Codex needs nothing installed. Speech models come in the next step.",
        "Codex 无需安装任何内容。语音模型在下一步选择。",
        "Codex 無須安裝任何內容。語音模型在下一步選擇。",
        "Codex 用のインストールは不要です。音声モデルは次のステップで選びます。",
    ),
    ("Case colour", "外壳颜色", "外殼顏色", "本体カラー"),
    (
        "So the picture matches your deck.",
        "让图示与你的设备一致。",
        "讓圖片與你的裝置一致。",
        "画像をお使いのデバイスに合わせます。",
    ),
    ("Add These", "添加这些项目", "新增這些項目", "これらを追加"),
    ("Not Now", "以后再说", "稍後再說", "今はしない"),
    (
        "Each can be removed later in Service.",
        "之后可在“服务”中逐项移除。",
        "之後可在「服務」中逐項移除。",
        "あとで「サービス」から削除できます。",
    ),
    (
        "Adding the background service…",
        "正在添加后台服务…",
        "正在新增背景服務…",
        "バックグラウンドサービスを追加しています…",
    ),
    ("Waiting for the service…", "正在等待服务…", "正在等待服務…", "サービスを待っています…"),
    (
        "Adding the Claude Code plugin…",
        "正在添加 Claude Code 插件…",
        "正在新增 Claude Code 外掛…",
        "Claude Code プラグインを追加しています…",
    ),
    (
        "macOS is waiting for your approval in System Settings > General > Login Items.",
        "macOS 正在等待你在“系统设置 > 通用 > 登录项”中批准。",
        "macOS 正在等待你在「系統設定 > 一般 > 登入項目」中核准。",
        "macOS は「システム設定 > 一般 > ログイン項目」での承認を待っています。",
    ),
    (
        "The service did not start. Open Service to see why.",
        "服务没有启动。请打开“服务”查看原因。",
        "服務沒有啟動。請開啟「服務」查看原因。",
        "サービスが起動しませんでした。「サービス」で原因を確認してください。",
    ),
    (
        "The plugin was not added: {0}",
        "未能添加插件：{0}",
        "未能新增外掛：{0}",
        "プラグインを追加できませんでした: {0}",
    ),
    // Steps 3 and 4: the Voice and Integrations pages' cards, introduced.
    // The sheet's sentence, and a second line that says where a Conol
    // chat's speech goes: the Conol switch in the cards below is on unless
    // the owner turns it off.
    (
        "Speech is recognised and spoken on this Mac unless you choose otherwise.\nWhile the Conol switch below is on, what you say to Conol chats is heard by Conol's cloud.",
        "除非你另行选择，语音的识别和朗读都在此 Mac 上完成。\n下方的 Conol 开关开启时，你对 Conol 聊天说的话由 Conol 云端识别。",
        "除非你另行選擇，語音的辨識和朗讀都在此 Mac 上完成。\n下方的 Conol 開關開啟時，你對 Conol 聊天說的話由 Conol 雲端辨識。",
        "ほかを選ばない限り、音声の認識と読み上げはこの Mac 上で行います。\n下の Conol のスイッチがオンの間、Conol チャットへの発話は Conol クラウドが認識します。",
    ),
    (
        "What can put sessions on your deck.",
        "哪些来源可以把会话送到你的设备上。",
        "哪些來源可以把工作階段送到你的裝置上。",
        "デバイスにセッションを届けられる接続元。",
    ),
    ("Finish", "完成", "完成", "完了"),
    // Added by this page (not in the sheet): a deck found on a bus the
    // probe cannot name. The same words the two lines above begin with.
    ("Deck found", "已找到设备", "已找到裝置", "デバイスが見つかりました"),
    // And the button beside macOS's approval line: the Login Items pane,
    // by the name System Settings gives it.
    ("Open Login Items", "打开登录项", "開啟登入項目", "ログイン項目を開く"),
];
