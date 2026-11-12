//! The Voice page's words, each as `(English, 简体中文, 繁體中文, 日本語)`.
//! English is the key.
//!
//! The rows are the voice sheet's (`voice.tsv` of the string sheets), copied
//! as written and in its order. A word the page shares with another page
//! (Installed, Granted, Bluetooth, the older service's sentence) is in
//! `crate::i18n::kit`, and one the built-in tables already ship (Voice,
//! Recognition, Spoken replies, Cancel, Retry, Failed, Provider, the kept
//! toasts of the engine downloads and the key) is not repeated: one key has
//! one set of words wherever it is. The names that are never translated
//! (Apple, Whisper, Kokoro, OpenAI, ElevenLabs, Conol) have no row.
use deck_gui_core::i18n::Entry;

pub const STRINGS: &[Entry] = &[
    // The page's head, and the language the recognisers hear.
    (
        "Push to talk, and replies read on the deck's speaker.",
        "按住说话，回复由设备扬声器朗读。",
        "按住說話，回覆由裝置揚聲器朗讀。",
        "押して話す機能と、返信をデバイスのスピーカーで読み上げる機能。",
    ),
    (
        "Speech language",
        "语音识别语言",
        "語音辨識語言",
        "音声認識の言語",
    ),
    (
        "Whisper detects the language. Apple's recogniser needs one chosen here.",
        "Whisper 会自动检测语言；Apple 识别器需要在此指定一种。",
        "Whisper 會自動偵測語言；Apple 辨識器需要在此指定一種。",
        "Whisper は言語を自動で判別します。Apple の認識では、ここで 1 つ選びます。",
    ),
    ("Automatic", "自动", "自動", "自動"),
    // The Speech language popup's names for the recognisers' locales; a
    // locale with no row here is shown by its code.
    (
        "English (United States)",
        "英语（美国）",
        "英文（美國）",
        "英語（アメリカ合衆国）",
    ),
    (
        "English (United Kingdom)",
        "英语（英国）",
        "英文（英國）",
        "英語（イギリス）",
    ),
    (
        "Chinese (China mainland)",
        "中文（中国大陆）",
        "中文（中國大陸）",
        "中国語（中国本土）",
    ),
    (
        "Chinese (Taiwan)",
        "中文（台湾）",
        "中文（台灣）",
        "中国語（台湾）",
    ),
    (
        "Cantonese (Hong Kong)",
        "粤语（香港）",
        "粵語（香港）",
        "広東語（香港）",
    ),
    (
        "Cantonese (China mainland)",
        "粤语（中国大陆）",
        "粵語（中國大陸）",
        "広東語（中国本土）",
    ),
    (
        "Japanese (Japan)",
        "日语（日本）",
        "日文（日本）",
        "日本語（日本）",
    ),
    (
        "Korean (South Korea)",
        "韩语（韩国）",
        "韓文（南韓）",
        "韓国語（韓国）",
    ),
    (
        "German (Germany)",
        "德语（德国）",
        "德文（德國）",
        "ドイツ語（ドイツ）",
    ),
    (
        "French (France)",
        "法语（法国）",
        "法文（法國）",
        "フランス語（フランス）",
    ),
    (
        "Spanish (Spain)",
        "西班牙语（西班牙）",
        "西班牙文（西班牙）",
        "スペイン語（スペイン）",
    ),
    (
        "Italian (Italy)",
        "意大利语（意大利）",
        "義大利文（義大利）",
        "イタリア語（イタリア）",
    ),
    (
        "Portuguese (Brazil)",
        "葡萄牙语（巴西）",
        "葡萄牙文（巴西）",
        "ポルトガル語（ブラジル）",
    ),
    (
        "Russian (Russia)",
        "俄语（俄罗斯）",
        "俄文（俄羅斯）",
        "ロシア語（ロシア）",
    ),
    (
        "Dutch (Netherlands)",
        "荷兰语（荷兰）",
        "荷蘭文（荷蘭）",
        "オランダ語（オランダ）",
    ),
    (
        "Polish (Poland)",
        "波兰语（波兰）",
        "波蘭文（波蘭）",
        "ポーランド語（ポーランド）",
    ),
    (
        "Turkish (Türkiye)",
        "土耳其语（土耳其）",
        "土耳其文（土耳其）",
        "トルコ語（トルコ）",
    ),
    (
        "Vietnamese (Vietnam)",
        "越南语（越南）",
        "越南文（越南）",
        "ベトナム語（ベトナム）",
    ),
    // Recognition: the two recognisers, a local model's states, and the
    // switch that lets Conol hear the presses meant for its chats. An
    // engine's title shares its line with what its row offers (Download ·
    // 365 MB takes about 160 of a 372 px card in Japanese), so Japanese
    // says Mac 上 there, without この, and never wraps its closing bracket
    // onto a line of its own; the sentences keep この Mac 上.
    (
        "Apple, on this Mac",
        "Apple（在此 Mac 上）",
        "Apple（在此 Mac 上）",
        "Apple（Mac 上）",
    ),
    (
        "Built in. One language at a time.",
        "内置。一次只能识别一种语言。",
        "內建。一次只能辨識一種語言。",
        "内蔵。一度に 1 言語のみ。",
    ),
    (
        "Whisper, on this Mac",
        "Whisper（在此 Mac 上）",
        "Whisper（在此 Mac 上）",
        "Whisper（Mac 上）",
    ),
    (
        "Many languages, mixed speech.",
        "支持多种语言，可混合识别。",
        "支援多種語言，可在一句話中混用。",
        "多言語・言語混在に対応。",
    ),
    (
        "Download · {0}",
        "下载 · {0}",
        "下載 · {0}",
        "ダウンロード · {0}",
    ),
    (
        "Downloading {0}%",
        "正在下载 {0}%",
        "正在下載 {0}%",
        "ダウンロード中 {0}%",
    ),
    ("Loading…", "正在加载…", "正在載入…", "読み込み中…"),
    (
        "Conol's cloud, for Conol chats only",
        "Conol 云端（仅用于 Conol 聊天）",
        "Conol 雲端（僅用於 Conol 聊天）",
        "Conol クラウド（Conol チャット専用）",
    ),
    (
        "Audio of those presses goes to Conol's service.\nOff keeps all recognition on this Mac.",
        "这部分语音会发送到 Conol 的服务。\n关闭后，所有识别都在此 Mac 上完成。",
        "這部分語音會傳送到 Conol 的服務。\n關閉後，所有辨識都在此 Mac 上完成。",
        "その音声は Conol のサービスに送信されます。\nオフなら認識はすべてこの Mac 上で行います。",
    ),
    // Spoken replies: the three voices, the key dialog, and the local
    // model only the command line sets up.
    ("Apple voice", "Apple 语音", "Apple 語音", "Apple の音声"),
    ("Built in.", "内置。", "內建。", "内蔵。"),
    (
        "Kokoro, on this Mac",
        "Kokoro（在此 Mac 上）",
        "Kokoro（在此 Mac 上）",
        "Kokoro（Mac 上）",
    ),
    (
        "Chinese and English.",
        "中文和英语。",
        "中文與英語。",
        "中国語と英語。",
    ),
    (
        "Cloud voice, with your own key",
        "云端语音（自备密钥）",
        "雲端語音（自備金鑰）",
        "クラウド音声（自分のキー）",
    ),
    (
        "OpenAI or ElevenLabs.\nThe reply text is sent to them.",
        "OpenAI 或 ElevenLabs。\n回复文本会发送给服务提供商。",
        "OpenAI 或 ElevenLabs。\n回覆文字會傳送給服務供應商。",
        "OpenAI または ElevenLabs。\n返信文をプロバイダーに送ります。",
    ),
    ("Add Key", "添加密钥", "新增金鑰", "キーを追加"),
    ("Change…", "更改…", "變更…", "変更…"),
    (
        "Cloud voice key",
        "云端语音密钥",
        "雲端語音金鑰",
        "クラウド音声のキー",
    ),
    (
        "The key is kept in the service's settings on this Mac.",
        "密钥保存在此 Mac 上的服务设置中。",
        "金鑰儲存在此 Mac 上的服務設定中。",
        "キーはこの Mac のサービス設定に保存されます。",
    ),
    ("Save Key", "保存密钥", "儲存金鑰", "キーを保存"),
    ("Remove Key", "移除密钥", "移除金鑰", "キーを削除"),
    (
        "Qwen3-TTS (MLX), on this Mac",
        "Qwen3-TTS（MLX，在此 Mac 上）",
        "Qwen3-TTS（MLX，在此 Mac 上）",
        "Qwen3-TTS（MLX、Mac 上）",
    ),
    (
        "Set up from the command line.",
        "请通过命令行设置。",
        "請透過命令列設定。",
        "コマンドラインで設定します。",
    ),
    // Where a press and a reply travel, and the test from the Mac.
    ("Signal path", "语音通路", "語音路徑", "音声経路"),
    ("Agent sessions", "助手会话", "助理工作階段", "エージェント"),
    ("Conol chats", "Conol 聊天", "Conol 聊天", "Conol チャット"),
    ("Deck microphone", "设备麦克风", "裝置麥克風", "本体マイク"),
    ("The agent", "助手", "助理", "エージェント"),
    ("Deck speaker", "设备扬声器", "裝置揚聲器", "本体スピーカー"),
    (
        "Conol's cloud",
        "Conol 云端",
        "Conol 雲端",
        "Conol クラウド",
    ),
    ("The chat", "聊天", "聊天", "チャット"),
    (
        "Test Voice asks on the deck: hold PTT and say hello.",
        "“测试语音”会在设备上提示：按住 PTT 说声你好。",
        "「測試語音」會在裝置上提示：按住 PTT 說聲你好。",
        "「音声をテスト」を押し、デバイスで PTT を押しながら話します。",
    ),
    (
        "Waiting for you to hold PTT on the deck…",
        "正在等待你按住设备上的 PTT…",
        "正在等待你按住裝置上的 PTT…",
        "デバイスの PTT が押されるのを待っています…",
    ),
    (
        "The last test heard “{0}”.",
        "上次测试听到的是“{0}”。",
        "上次測試聽到的是「{0}」。",
        "前回のテストでは「{0}」と聞き取りました。",
    ),
    (
        "The last test heard nothing.",
        "上次测试没有听到任何内容。",
        "上次測試沒有聽到任何內容。",
        "前回のテストでは何も聞き取れませんでした。",
    ),
    ("Test Voice", "测试语音", "測試語音", "音声をテスト"),
    ("Speak a Sample", "朗读示例", "朗讀範例", "サンプルを再生"),
    ("Play a Chime", "播放提示音", "播放提示音", "通知音を再生"),
    // The permissions the deck's voice needs on this Mac.
    (
        "macOS permissions",
        "macOS 权限",
        "macOS 權限",
        "macOS の権限",
    ),
    (
        "For the deck's microphone and speaker when the cable is out.",
        "未接数据线时，用于设备的麦克风和扬声器。",
        "未接上連接線時，用於裝置的麥克風和揚聲器。",
        "ケーブルを外しているとき、デバイスのマイクとスピーカーに使います。",
    ),
    ("Not asked", "尚未请求", "尚未請求", "未要求"),
    ("Denied", "已拒绝", "已拒絕", "拒否済み"),
    ("Speech Recognition", "语音识别", "語音辨識", "音声認識"),
    (
        "Only for Apple's recogniser.",
        "仅 Apple 识别器需要。",
        "僅 Apple 辨識器需要。",
        "Apple の認識を使う場合のみ必要です。",
    ),
    ("Ask…", "请求授权…", "請求授權…", "許可を求める…"),
];
