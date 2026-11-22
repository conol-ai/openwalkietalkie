//! **Context aware transcription** and **Model access** (`crate::context`),
//! each row as `(English, 简体中文, 繁體中文, 日本語)`. English is the key.
//! Words another table already ships (Cancel, Reading…, Remove Key, Save
//! Key, Paste the key first., the older service's sentence) are not
//! repeated. Plain words: no "LLM", no internal names. The feature's Chinese
//! name is 按窗口改写听写 ("rewrite dictation for the window"), the same in
//! every row that names it.
use deck_gui_core::i18n::Entry;

pub const STRINGS: &[Entry] = &[
    ("Context aware transcription", "按窗口改写听写", "按視窗改寫聽寫", "ウィンドウに合わせた口述の書き直し"),
    ("Experimental", "实验性", "實驗性", "試験的"),
    ("What does experimental mean?", "实验性是什么意思？", "實驗性是什麼意思？", "「試験的」とは？"),
    (
        "Rewrite what I dictate for the window it goes into",
        "把我口述的内容改写成适合目标窗口的文字",
        "把我口述的內容改寫成適合目標視窗的文字",
        "口述した内容を入力先のウィンドウに合わせて書き直す",
    ),
    // The disclosure, under the switch.
    (
        "What you dictate is sent to the models chosen below. Off until you turn it on.",
        "你口述的内容会发送到下方选择的模型。在你开启之前一直是关闭的。",
        "你口述的內容會傳送到下方選擇的模型。在你開啟之前一直是關閉的。",
        "口述した内容は、下で選んだモデルに送信されます。オンにするまではオフのままです。",
    ),
    // The experimental note.
    (
        "It may leave your words unchanged without saying so: when a model is slow, unreachable or answers oddly, what you said is typed as heard.",
        "它可能不提示就保留你的原话：模型太慢、连不上或回答异常时，会按听到的原样输入。",
        "它可能不提示就保留你的原話：模型太慢、連不上或回答異常時，會按聽到的原樣輸入。",
        "知らせずに言葉をそのまま残すことがあります。モデルが遅い、つながらない、おかしな答えを返したときは、聞き取ったとおりに入力されます。",
    ),
    (
        "Its settings may change in a later version.",
        "它的设置可能会在以后的版本中改变。",
        "它的設定可能會在之後的版本中改變。",
        "設定は今後のバージョンで変わることがあります。",
    ),
    (
        "Some setups are untested: the window readers for macOS and Windows have never been built, no model running on this computer has been tried yet, and Anthropic has only been tried against a stand-in.",
        "有些环境还没有测试过：macOS 和 Windows 上读取窗口的部分从未编译过，还没有试过在本机运行的模型，Anthropic 也只用模拟服务测试过。",
        "有些環境還沒有測試過：macOS 和 Windows 上讀取視窗的部分從未編譯過，還沒有試過在本機執行的模型，Anthropic 也只用模擬服務測試過。",
        "まだ試していない環境があります。macOS と Windows のウィンドウ読み取りは一度もビルドされておらず、このコンピュータ上で動くモデルはまだ試しておらず、Anthropic は代わりのサーバーでしか試していません。",
    ),
    ("Got it", "知道了", "知道了", "わかりました"),
    (
        "It is on, but no rewriting model is chosen, so what you dictate is typed exactly as heard.",
        "已开启，但还没有选择改写模型，所以口述的内容会按听到的原样输入。",
        "已開啟，但還沒有選擇改寫模型，所以口述的內容會按聽到的原樣輸入。",
        "オンですが、書き直しのモデルが選ばれていないため、口述した内容は聞き取ったとおりに入力されます。",
    ),
    // The two pickers.
    ("Window recognition model", "窗口识别模型", "視窗辨識模型", "ウィンドウ判別モデル"),
    ("Rewriting model", "改写模型", "改寫模型", "書き直しのモデル"),
    ("Choose the window recognition model", "选择窗口识别模型", "選擇視窗辨識模型", "ウィンドウ判別モデルを選ぶ"),
    ("Choose the rewriting model", "选择改写模型", "選擇改寫模型", "書き直しのモデルを選ぶ"),
    ("Rules only, no model", "只用规则，不用模型", "只用規則，不用模型", "ルールのみ（モデルなし）"),
    ("None: typed exactly as heard", "不改写：按听到的原样输入", "不改寫：按聽到的原樣輸入", "なし：聞き取ったとおりに入力"),
    ("Choose\u{2026}", "选择\u{2026}", "選擇\u{2026}", "選ぶ\u{2026}"),
    ("Own address", "自有地址", "自有位址", "独自のアドレス"),
    ("Local \u{00b7} slower", "本地 \u{00b7} 较慢", "本機 \u{00b7} 較慢", "ローカル \u{00b7} 遅め"),
    ("Search models", "搜索模型", "搜尋模型", "モデルを検索"),
    ("{0} more: type to narrow the list", "还有 {0} 个：输入文字缩小范围", "還有 {0} 個：輸入文字縮小範圍", "ほか {0} 件：入力して絞り込み"),
    ("Model id", "模型 ID", "模型 ID", "モデル ID"),
    ("Use", "使用", "使用", "使う"),
    ("Add more\u{2026}", "添加更多\u{2026}", "新增更多\u{2026}", "さらに追加\u{2026}"),
    // Model access.
    ("Model access", "模型接入", "模型接入", "モデルへの接続"),
    (
        "Providers and their keys, set once and shared by every feature.",
        "服务商和它们的密钥：设置一次，所有功能共用。",
        "服務商和它們的金鑰：設定一次，所有功能共用。",
        "プロバイダとそのキー。一度設定すれば、すべての機能で共有されます。",
    ),
    ("Open", "打开", "開啟", "開く"),
    ("No providers yet.", "还没有服务商。", "還沒有服務商。", "プロバイダがまだありません。"),
    ("Add provider", "添加服务商", "新增服務商", "プロバイダを追加"),
    ("Add", "添加", "新增", "追加"),
    ("Custom", "自定义", "自訂", "カスタム"),
    ("Address", "地址", "位址", "アドレス"),
    (
        "Enter the server's address, such as http://192.168.1.10:11434/v1.",
        "填写服务器地址，例如 http://192.168.1.10:11434/v1。",
        "填寫伺服器位址，例如 http://192.168.1.10:11434/v1。",
        "サーバーのアドレスを入力します（例：http://192.168.1.10:11434/v1）。",
    ),
    ("API key", "API 密钥", "API 金鑰", "API キー"),
    ("Paste the API key\u{2026}", "粘贴 API 密钥\u{2026}", "貼上 API 金鑰\u{2026}", "API キーを貼り付け\u{2026}"),
    ("{0}: API key", "{0}：API 密钥", "{0}：API 金鑰", "{0}：API キー"),
    ("Key {0}", "密钥 {0}", "金鑰 {0}", "キー {0}"),
    ("No key yet", "还没有密钥", "還沒有金鑰", "キーがまだありません"),
    ("Key saved", "已保存密钥", "已儲存金鑰", "キー保存済み"),
    ("Needs no key", "无需密钥", "無需金鑰", "キー不要"),
    ("Found on this computer", "在本机上找到", "在本機上找到", "このコンピュータで検出"),
    ("Built-in list", "内置列表", "內建列表", "組み込みの一覧"),
    ("{0} models", "{0} 个模型", "{0} 個模型", "{0} 件のモデル"),
    ("Set key", "设置密钥", "設定金鑰", "キーを設定"),
    ("Check connection", "检查连接", "檢查連線", "接続を確認"),
    ("Refresh models", "刷新模型列表", "重新整理模型列表", "モデル一覧を更新"),
    ("Remove provider", "移除服务商", "移除服務商", "プロバイダを削除"),
    // What a check found.
    ("Works", "可以使用", "可以使用", "使えます"),
    ("The key was refused", "密钥被拒绝", "金鑰被拒絕", "キーが拒否されました"),
    ("Could not reach it", "无法连接", "無法連線", "接続できません"),
    ("It does not have that model", "它没有这个模型", "它沒有這個模型", "そのモデルはありません"),
    ("It answered with an error", "它返回了错误", "它回傳了錯誤", "エラーが返されました"),
    // Toasts.
    ("Saved.", "已保存。", "已儲存。", "保存しました。"),
    ("Key removed.", "密钥已移除。", "金鑰已移除。", "キーを削除しました。"),
    ("Could not save: {0}", "无法保存：{0}", "無法儲存：{0}", "保存できませんでした：{0}"),
    // What a failed call is called in a toast (`shell::action_name`).
    ("Save the key", "保存密钥", "儲存金鑰", "キーを保存"),
    ("Remove the key", "移除密钥", "移除金鑰", "キーを削除"),
    ("Read the providers", "读取服务商", "讀取服務商", "プロバイダを読み込む"),
    ("Add the provider", "添加服务商", "新增服務商", "プロバイダを追加"),
    ("Remove the provider", "移除服务商", "移除服務商", "プロバイダを削除"),
    ("Check the provider", "检查服务商", "檢查服務商", "プロバイダを確認"),
    ("Refresh the models", "刷新模型列表", "重新整理模型列表", "モデル一覧を更新"),
];
