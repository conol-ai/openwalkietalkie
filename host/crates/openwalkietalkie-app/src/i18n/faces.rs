//! The Faces & Keys page's words, each as `(English, 简体中文, 繁體中文,
//! 日本語)`. English is the key.
//!
//! The rows are the faces sheet's (`faces.tsv` of the string sheets),
//! copied as written and in its order. A word the page shares with another
//! page (the page's name, the face names Macropad, System, Timer and
//! Arcade, the plate titles, Granted, Not checked, On the deck…) is in
//! `crate::i18n::kit`, and one the built-in tables already ship (Agents,
//! Usage, Settings, Remove, Cancel, None, Open Settings, Off) is not
//! repeated: one key has one set of words wherever it is. Of the deck's 73
//! key names only the six macOS names in words are here; the rest are
//! drawn as their caps print them.
use deck_gui_core::i18n::Entry;

pub const STRINGS: &[Entry] = &[
    // The page's head, the strip, and changing it.
    (
        "The strip you slide through with SHIFT and the wheel, and what each macro key types.",
        "按住 SHIFT 转动滚轮即可切换的页面列表，以及“快捷键”页面上每个按键发送的组合键。",
        "按住 SHIFT 轉動滾輪即可切換的頁面清單，以及「快捷鍵」頁面上每個按鍵傳送的組合鍵。",
        "SHIFT を押しながらホイールで切り替えるページ一覧と、マクロパッドの各キーが入力するショートカット。",
    ),
    ("Face strip", "页面列表", "頁面清單", "ページ一覧"),
    ("Add a face", "添加页面", "新增頁面", "ページを追加"),
    ("Move Left", "向左移动", "向左移動", "左へ移動"),
    ("Move Right", "向右移动", "向右移動", "右へ移動"),
    ("Remove from Deck…", "从设备移除…", "從裝置移除…", "デバイスから削除…"),
    (
        "Up to eight faces. Agents and Settings stay at the ends.",
        "最多八个页面。“助手”和“设置”固定在两端。",
        "最多八個頁面。「助理」和「設定」固定在兩端。",
        "最大 8 ページ。「エージェント」と「設定」は両端に固定。",
    ),
    (
        "Waiting for the deck…",
        "正在等待设备…",
        "正在等待裝置…",
        "デバイスを待っています…",
    ),
    (
        "The deck did not take the change. Close its face manager and try again.",
        "设备未接受此更改。请关闭设备上的页面管理后重试。",
        "裝置未接受此變更。請關閉裝置上的頁面管理後重試。",
        "デバイスが変更を受け付けませんでした。デバイスのページ管理を閉じてから、もう一度お試しください。",
    ),
    (
        "Link the deck to change its faces.",
        "连接设备后才能更改其页面。",
        "連線裝置後才能變更其頁面。",
        "ページを変更するには、デバイスを接続してください。",
    ),
    // The question before a face is removed.
    (
        "Remove {0} from the deck?",
        "要从设备移除“{0}”吗？",
        "要從裝置移除「{0}」嗎？",
        "「{0}」をデバイスから削除しますか？",
    ),
    (
        "Its keys are removed with it.",
        "它的按键会一并移除。",
        "它的按鍵會一併移除。",
        "そのキーも一緒に削除されます。",
    ),
    (
        "You can add it again later.",
        "之后可以再次添加。",
        "之後可以再次新增。",
        "あとで再び追加できます。",
    ),
    // The Macropad's keys, and the key selected among them.
    (
        "Macropad keys",
        "“快捷键”页面的按键",
        "「快捷鍵」頁面的按鍵",
        "マクロパッドのキー",
    ),
    (
        "Top left: the deck button. Top right: the shortcut it types.",
        "左上角：实体按键。右上角：发送的组合键。",
        "左上角：裝置按鍵。右上角：傳送的組合鍵。",
        "左上: 本体ボタン。右上: 入力するショートカット。",
    ),
    ("Assign", "分配", "指派", "割当"),
    ("Selected key", "所选按键", "所選按鍵", "選択中のキー"),
    ("Label", "标签", "標籤", "ラベル"),
    (
        "Up to 12 letters, or 4 Chinese characters.",
        "最多 12 个字母或 4 个汉字。",
        "最多 12 個字母或 4 個中文字。",
        "半角 12 文字、または全角 4 文字まで。",
    ),
    (
        "Too long. Up to 12 letters, or 4 Chinese characters.",
        "太长了。最多 12 个字母或 4 个汉字。",
        "太長了。最多 12 個字母或 4 個中文字。",
        "長すぎます。半角 12 文字、または全角 4 文字まで。",
    ),
    ("Deck button", "实体按键", "裝置按鍵", "本体ボタン"),
    ("Touch only", "仅触摸", "僅觸控", "タッチのみ"),
    ("Modifiers", "修饰键", "修飾鍵", "修飾キー"),
    ("Key", "按键", "按鍵", "キー"),
    ("Record", "录制", "錄製", "記録"),
    (
        "Press a key…",
        "请按下一个按键…",
        "請按下一個按鍵…",
        "キーを押してください…",
    ),
    (
        "The deck cannot send that key.",
        "设备无法发送这个按键。",
        "裝置無法傳送這個按鍵。",
        "デバイスはそのキーを送信できません。",
    ),
    // The six keys macOS names in words.
    ("Space", "空格键", "空白鍵", "スペース"),
    ("Up Arrow", "上箭头", "上箭頭", "上矢印"),
    ("Down Arrow", "下箭头", "下箭頭", "下矢印"),
    ("Left Arrow", "左箭头", "左箭頭", "左矢印"),
    ("Right Arrow", "右箭头", "右箭頭", "右矢印"),
    ("Forward Delete", "向前删除", "向前刪除", "前方削除"),
    // The grant the deck's shortcuts need.
    ("Accessibility", "辅助功能", "輔助使用", "アクセシビリティ"),
    (
        "Lets the deck type shortcuts.",
        "允许设备发送组合键。",
        "允許裝置傳送組合鍵。",
        "ショートカットの入力に必要です。",
    ),
    ("Check…", "检查…", "檢查…", "確認…"),
    ("Not allowed", "未允许", "未允許", "未許可"),
    (
        "Pick by hand the shortcuts\nmacOS reserves.",
        "macOS 保留的组合键\n请手动选择。",
        "macOS 保留的組合鍵\n請手動選擇。",
        "macOS のショートカットは\n手動で選びます。",
    ),
    ("Clear This Key", "清除此按键", "清除此按鍵", "このキーを消去"),
    (
        "Sent to the deck when it links.",
        "设备连接后发送。",
        "裝置連線後傳送。",
        "デバイスの接続時に送信します。",
    ),
    // The Usage face: where its bars come from.
    (
        "Usage face: where the bars come from",
        "“用量”页面：数据来源",
        "「用量」頁面：資料來源",
        "「使用量」ページ: データの取得元",
    ),
    ("Claude usage", "Claude 用量", "Claude 用量", "Claude の使用量"),
    (
        "Reads your Claude Code login from the Keychain to ask\nAnthropic for your usage. macOS asks once.",
        "从钥匙串读取你的 Claude Code 登录信息，\n向 Anthropic 查询用量。macOS 会询问一次。",
        "從鑰匙圈讀取你的 Claude Code 登入資訊，\n向 Anthropic 查詢用量。macOS 會詢問一次。",
        "キーチェーンの Claude Code ログインで Anthropic から\n使用量を取得します。macOS の確認は一度です。",
    ),
    ("Codex usage", "Codex 用量", "Codex 用量", "Codex の使用量"),
    (
        "From Codex. The session and weekly windows.",
        "来自 Codex。包括会话和每周两个用量周期。",
        "來自 Codex。包括工作階段和每週兩種用量週期。",
        "Codex から取得。セッション枠と週間枠。",
    ),
    (
        "One bar for each usage window.",
        "每个用量周期显示一条进度条。",
        "每個用量週期顯示一條進度列。",
        "使用量の枠ごとにバーを 1 本表示します。",
    ),
    // Under the two sources, from a service that asks only then
    // (`service.dormancy`).
    (
        "Asked only while this face is on the strip,\nand for up to ten minutes after the deck was last linked.",
        "仅当此页面在页面列表中时才会查询；\n设备断开连接后，最多再持续十分钟。",
        "僅當此頁面在頁面清單中時才會查詢；\n裝置中斷連線後，最多再持續十分鐘。",
        "このページがページ一覧にある間だけ取得します。\nデバイスの接続が切れてからは最大 10 分間です。",
    ),
    // The other faces' few settings.
    ("Demo data", "演示数据", "展示資料", "デモデータ"),
    (
        "Sample sessions on the deck while nothing is connected.",
        "未连接任何来源时，在设备上显示示例会话。",
        "未連線任何來源時，在裝置上顯示範例工作階段。",
        "何も接続していないとき、デバイスにサンプルのセッションを表示します。",
    ),
    ("Arcade sound", "游戏声音", "遊戲音效", "ゲーム音"),
    ("SFX only", "仅音效", "僅音效", "効果音のみ"),
    ("Music + SFX", "音乐和音效", "音樂與音效", "音楽＋効果音"),
    (
        "This face has no settings on the Mac.",
        "此页面在 Mac 上没有可设置的项目。",
        "此頁面在 Mac 上沒有可設定的項目。",
        "このページには Mac で変更できる設定はありません。",
    ),
    // The strip rule the service refused a strip by (`strip_problem`), in a
    // toast: one sentence for each of its words.
    (
        "The strip holds eight faces at most.",
        "页面列表最多容纳八个页面。",
        "頁面清單最多容納八個頁面。",
        "ページ一覧に置けるページは最大 8 つです。",
    ),
    (
        "Each face needs a number from 1 to 8.",
        "每个页面的编号须在 1 到 8 之间。",
        "每個頁面的編號須在 1 到 8 之間。",
        "各ページの番号は 1 から 8 である必要があります。",
    ),
    (
        "Two faces have the same number.",
        "有两个页面的编号相同。",
        "有兩個頁面的編號相同。",
        "番号が同じページが 2 つあります。",
    ),
    (
        "Every face needs a name.",
        "每个页面都需要名称。",
        "每個頁面都需要名稱。",
        "各ページに名前が必要です。",
    ),
    (
        "Agents must be the first face.",
        "“助手”必须是第一个页面。",
        "「助理」必須是第一個頁面。",
        "「エージェント」は最初のページである必要があります。",
    ),
    (
        "Settings must be the last face.",
        "“设置”必须是最后一个页面。",
        "「設定」必須是最後一個頁面。",
        "「設定」は最後のページである必要があります。",
    ),
    (
        "Agents and Settings cannot be in the middle.",
        "“助手”和“设置”不能放在中间。",
        "「助理」和「設定」不能放在中間。",
        "「エージェント」と「設定」は中間に置けません。",
    ),
    (
        "The strip can hold three Macropads at most.",
        "页面列表最多容纳三个“快捷键”页面。",
        "頁面清單最多容納三個「快捷鍵」頁面。",
        "ページ一覧に置けるマクロパッドは最大 3 つです。",
    ),
    (
        "The strip can hold one Timer.",
        "页面列表只能容纳一个“计时器”页面。",
        "頁面清單只能容納一個「計時器」頁面。",
        "ページ一覧に置けるタイマーは 1 つだけです。",
    ),
];
