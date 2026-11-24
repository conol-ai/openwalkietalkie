# Website terminology glossary: zh-Hans, zh-Hant, ja

Use this glossary when you translate the OpenWalkieTalkie website. The site has
to use the same words as the product people hold and install, so these
translations come from the product's own string tables, not from a
translator's instinct.

**Rules**

1. Use these words. If a term is listed, use the listed form exactly, with the
   same punctuation and spacing.
2. If a term is not listed, search the app tables (the files below) before you
   make one up. Only coin a word when the app has none, and follow the
   "Style observed in the app" section when you do.
3. zh-Hant is Traditional Chinese in Taiwan usage, as the app writes it. It is
   not zh-Hans run through a converter: the vocabulary differs (裝置, 韌體,
   連線, 檔案, 程式, 外掛, 權杖, 金鑰…). Never derive zh-Hant from zh-Hans by
   character conversion.
4. Where the app is inconsistent, the table lists both forms and the
   recommended one for the website.

**Source files** (each is a list of `(English, zh-Hans, zh-Hant, ja)` tuples).
The short names are used in the table:

| Short name | Path |
|---|---|
| core/common, core/app, core/auxiliary, core/platform | `host/crates/deck-gui-core/src/i18n/{common,app,auxiliary,platform}.rs` (desktop, shared) |
| app/kit, app/frame, app/deck, app/faces, app/firstrun, app/integrations, app/voice, app/context, app/service | `host/crates/openwalkietalkie-app/src/i18n/*.rs` (the companion app) |
| fw/common, fw/agents, fw/faces, fw/overlays, fw/settings | `src/ui/i18n/*.rs` (the deck's own screen) |
| language.rs | `src/language.rs` (language names) |

## 1. Main table

Quoted strings are copied exactly from the files. "Label" means a UI label
that was quoted on its own; sentences are quoted to show a term in context.

### The product and its parts

| English | zh-Hans | zh-Hant | ja | Source | Note |
|---|---|---|---|---|---|
| Deck (the device) | 设备 | 裝置 | デバイス | core/common `Deck`; app/deck `Your deck` → 你的设备 / 你的裝置 / お使いのデバイス | **The app never calls the deck "deck".** It is always 设备 / 裝置 / デバイス. A product name like "Deck" does not appear in CJK. |
| the device (generic) | 设备 | 裝置 | デバイス | core/auxiliary `I understand — update device` | Same word as "deck". |
| on the deck (hardware part) | 设备 / 实体 | 裝置 | 本体 | app/faces `Deck button` 实体按键 / 裝置按鍵 / 本体ボタン; app/voice `Deck microphone` 设备麦克风 / 裝置麥克風 / 本体マイク, `Deck speaker` 设备扬声器 / 裝置揚聲器 / 本体スピーカー | For a physical part, ja uses 本体; zh-Hans uses 实体 only for the button. |
| OpenWalkieTalkie | OpenWalkieTalkie | OpenWalkieTalkie | OpenWalkieTalkie | core/platform `Open OpenWalkieTalkie` 打开 OpenWalkieTalkie / 開啟 OpenWalkieTalkie / OpenWalkieTalkie を開く | Always Latin, never translated. |
| the app / companion app | 应用 | 應用程式 | アプリ | app/service `The app and the service update together.` 应用和服务会一起更新。 / 應用程式和服務會一起更新。 / アプリとサービスは一緒に更新されます。 | The app has no word for "companion". Use 应用 / 應用程式 / アプリ. "Mac app" = `Mac 应用` / `Mac 應用程式` / `Macアプリ` (fw/agents). |
| this app | 此应用 / 本应用 | 此應用程式 / 本應用程式 | このアプリ | app/firstrun `What this app adds`; core/platform | |
| service (deckd) | 服务 | 服務 | サービス | core/common `Service` | Page name and short form. deckd stays Latin: `deckd，位于“系统设置”的“登录项”中` (app/service). |
| background service | 后台服务 | 背景服務 | バックグラウンドサービス | app/kit `Background service` | The long form, used when the reader may not know what "the service" is. Recommended on the website for the first mention. |
| Deck host (deck's view of the service) | 配套服务 | 配套服務 | 連携サービス | fw/settings `Deck host` | Only on the deck's About screen. Do not use on the site. |
| host / computer (as the deck sees it) | 主机 / 电脑 | 主機 / 電腦 | ホスト / パソコン | fw/settings `Host` 主机 / 主機 / ホスト; fw/overlays `NO HOST` 电脑未连接 / 電腦未連線 / 未接続 | The deck says 电脑 / 電腦 / パソコン in sentences. |
| firmware | 固件 | 韌體 | ファームウェア | fw/settings `Firmware`; app/deck | |
| deck firmware | 设备固件 | 裝置韌體 | デバイスのファームウェア / ファームウェア | app/service `Deck firmware {0}`; app/deck `Deck firmware {0} is available` 设备固件 {0} 现已可用 / 裝置韌體 {0} 現已可用 / ファームウェア {0} が利用可能 | |
| bootloader | 引导程序 | 開機載入程式 | ブートローダー | core/auxiliary | |
| board (PCB) / revision | 主板 / 版本 | 主機板 / 版本 | 基板 / リビジョン | core/auxiliary `The device cannot report its board. Choose the revision printed on your board.` | Use 主板 / 主機板 / 基板 for "the board". |
| case / case colour | 外壳 / 外壳颜色 | 外殼 / 外殼顏色 | 本体カラー | app/deck `Case`; app/firstrun `Case colour` | ja uses 本体カラー for both. White / Black = 白色 / 白色 / ホワイト, 黑色 / 黑色 / ブラック (app/kit). |
| hardware repair | 硬件维修 | 硬體維修 | ハードウェア修理 | core/auxiliary (bootloader warning) | Shows 硬件 / 硬體 / ハードウェア. |
| software shipped with the deck | 设备附带的软件 | 裝置隨附的軟體 | デバイス付属のソフト | core/platform, app/service | Shows 软件 / 軟體 / ソフト. |
| screen | 屏幕 | 螢幕 | 画面 | app/deck `If the screen is dark` 如果屏幕不亮 / 如果螢幕不亮 / 画面が消えているとき | |
| wheel | 滚轮 | 滾輪 | ホイール | fw/overlays `WHEEL`, `Turn the wheel` 转动滚轮 / 轉動滾輪 / ホイールを回す | |
| key (a physical / macro key) | 按键 | 按鍵 | キー | fw/overlays `Key`; app/faces `Key`, `Selected key` 所选按键 / 所選按鍵 / 選択中のキー | See "API key" below: ja uses キー for both. |
| button (deck button) | 实体按键 | 裝置按鍵 | 本体ボタン | app/faces `Deck button` | The app has no separate "button" word for the deck; keys are 按键 / 按鍵 / キー. |
| key names | PTT, SHIFT, ENTER, CANCEL | same | same | fw/overlays | Physical key names stay in Latin capitals as printed on the caps. |
| battery | 电池 (label) / 电量 (level) | 電池 / 電量 | バッテリー | fw/settings `Battery`; app/deck `Battery` and head line 连接、电量和各项设置 / 連線、電量和各項設定 | Comment in app/deck: the label is 电池 / 電池, not the board's 电量. Use 电量 / 電量 only for charge level. |
| charging / full | 充电中 / 已充满 | 充電中 / 已充滿 | 充電中 / 満充電 | fw/settings | |
| speaker | 扬声器 | 揚聲器 | スピーカー | fw/settings `Speaker`; app/deck | |
| microphone | 麦克风 | 麥克風 | マイク | core/platform `microphone`; app/voice `Deck microphone` | `Mic gain` = 麦克风增益 / 麥克風增益 / マイク感度. |
| volume | 音量 | 音量 | 音量 | fw/settings `Volume` | `Volume and speech` = 音量与播报 / 音量與播報 / 音量と音声 (app/deck). |
| brightness | 亮度 | 亮度 | 明るさ | fw/settings `Brightness` | |
| display (settings panel) | 显示 | 顯示 | 表示 | fw/settings `Display` | |
| text size | 文字大小 | 文字大小 | 文字サイズ | fw/settings | |
| power off | 关机 | 關機 | 電源オフ | fw/settings `Power off`; fw/overlays `Power off?` 关机？/ 關機？/ 電源を切りますか？ | |
| factory reset | 恢复出厂设置 | 恢復原廠設定 (settings) / 回復原廠設定 (overlay) | 初期化 | fw/settings `Factory reset`; fw/overlays `Factory reset?` 回復原廠設定？ | **Inconsistent in zh-Hant.** Recommend 恢復原廠設定 (the Settings label). |
| factory image | — | — | — | not in the app | See section 3. |
| recovery mode | 恢复模式 | 復原模式 | リカバリーモード | core/auxiliary `Recovery mode` | |
| locked (screen lock) | 已锁定 | 已鎖定 | ロック中 | fw/overlays `Locked` | The only lock word in the app. "Screen lock" is not in the app; see section 3. |
| tutorial | 教程 / 使用教程 | 教學 / 使用教學 | 使い方 | fw/overlays `TUTORIAL` 教程; fw/settings `Tutorial` 使用教程 | Menu label is 使用教程 / 使用教學. |
| language | 语言 | 語言 | 言語 | core/common, fw/settings `Language` | |
| language names | 简体中文 | 繁體中文 | 日本語 | language.rs `name()` | Each name is written in its own script, in every UI language. English is `English`. Codes: `zh-Hans`, `zh-Hant`, `ja`. |
| About | 关于 | 關於 | 情報 | fw/settings `About` | ja is 情報, not バージョン情報. |
| theme | — | — | — | not in the app | See section 3. |

### Faces (pages of the deck)

| English | zh-Hans | zh-Hant | ja | Source | Note |
|---|---|---|---|---|---|
| face | 页面 | 頁面 | ページ | fw/common `face`, `FACES`; fw/overlays `add a face` 添加页面 / 新增頁面 / ページを追加 | **"Face" is always 页面 / 頁面 / ページ.** Never 表盘, 面, 画面. |
| face strip | 页面列表 | 頁面清單 | ページ一覧 | app/faces `Face strip` | |
| face manager | 页面管理 | 頁面管理 | ページ管理 | fw/overlays `Two quick SHIFT taps open the face manager.` | |
| the {name} face | “{0}”页面 | 「{0}」頁面 | 「{0}」ページ | app/deck `Showing the {0} face`; app/faces `Usage face: where the bars come from` “用量”页面：数据来源 | Face names are usually quoted when used in a sentence. |
| Agents (face) | 助手 | 助理 | エージェント | fw/common `Agents`, `AGENTS`; core/common | Same word as "agents" in general. |
| Macropad | 快捷键 | 快捷鍵 | マクロパッド | fw/common `Macropad`; app/kit | In a sentence zh quotes it: `“快捷键”页面的按键` / `「快捷鍵」頁面的按鍵` (app/faces `Macropad keys`); ja `マクロパッドのキー`. |
| Usage | 用量 | 用量 | 使用量 | fw/common `Usage`; core/common | |
| System | 系统 | 系統 | システム | fw/common `System`; app/kit | `SYSTEM STATUS` = 系统状态 / 系統狀態 / システム状態 (fw/faces). |
| Arcade | 游戏 | 遊戲 | ゲーム | fw/common `Arcade`; app/kit | The games: `FLAPPY BIRD` 像素小鸟 / 像素小鳥 / フラッピーバード, `SNAKE` 贪吃蛇 / 貪食蛇 / スネーク, `RAIDEN` 雷电 / 雷電 / 雷電 (fw/faces). |
| Timer | 计时器 | 計時器 | タイマー | fw/common `Timer`; app/kit | |
| Stopwatch | 秒表 | 碼錶 | ストップウォッチ | fw/faces | |
| Pomodoro | 番茄钟 | 番茄鐘 | ポモドーロ | fw/faces `Pomodoro` | Focus / Short break / Long break = 专注 / 短休息 / 长休息, 專注 / 短休息 / 長休息, 集中 / 小休憩 / 長い休憩. |
| Settings (face) | 设置 | 設定 | 設定 | fw/common `Settings` | |
| Omarchy face | Omarchy 页面 | Omarchy 頁面 | Omarchy のページ | app/integrations `Sessions, the Omarchy\nface and its keys.` 会话、Omarchy 页面\n及其按键。 / 工作階段、Omarchy 頁面\n及其按鍵。 / セッション、Omarchy の\nページとキー。 | The face's own title `OMARCHY` has no table row and stays Latin. |
| workspace / windows | 工作区 / 窗口 | 工作區 / 視窗 | ワークスペース / ウィンドウ | fw/faces `workspace`, `windows`; fw/overlays `workspaces and windows` 工作区和窗口 / 工作區與視窗 / ワークスペースとウィンドウ | |
| face descriptions (gallery) | 配置文件中的九个按键; 订阅用量限额; 三个随身小游戏; 电脑资源; 秒表和倒计时 | 設定檔中的九個按鍵; 訂閱用量上限; 三個隨身小遊戲; 電腦資源; 碼錶與倒數計時 | 設定ファイルの9キー; サブスクリプションの使用量; 3つのミニゲーム; パソコンのリソース; ストップウォッチとタイマー | fw/overlays `nine keys from your dotfile`, `subscription limits`, `three pocket games`, `computer resources`, `stopwatch and countdown` | Good one-line descriptions to reuse on the Faces section of the site. |

### Talking to the deck: voice, PTT, dictation

| English | zh-Hans | zh-Hant | ja | Source | Note |
|---|---|---|---|---|---|
| Voice (page / feature) | 语音 | 語音 | 音声 | core/common `Voice` | Voice page head: `Push to talk, and replies read on the deck's speaker.` 按住说话，回复由设备扬声器朗读。 / 按住說話，回覆由裝置揚聲器朗讀。 / 押して話す機能と、返信をデバイスのスピーカーで読み上げる機能。 (app/voice) |
| push to talk | 按住说话 | 按住說話 | 押して話す | fw/overlays `Push to talk` | |
| PTT | PTT | PTT | PTT | fw/overlays `Hold PTT` 按住 PTT / 按住 PTT / PTT を押す | Always Latin. Describe it as `PTT（左侧的长条按键）` / `PTT（左側邊緣的長條按鍵）` / `PTT（左側面の長いキー）` (app/deck). |
| talk / speak | 说话 | 說話 | 話す | fw/common `talk` | |
| transcription / transcribe | 转写 | 轉寫 | 文字起こし | fw/overlays `Transcription failed` 转写失败 / 轉寫失敗 / 文字起こしに失敗; `Transcribing…` 正在转写… / 正在轉寫… / 文字起こし中… | Speech to text on the deck. |
| dictation / dictate | 听写 (noun, feature name) / 口述 (verb) | 聽寫 / 口述 | 口述 | app/context `Rewrite what I dictate…` 把我口述的内容… / 把我口述的內容… / 口述した内容を…; feature name uses 听写 / 聽寫 | Typing at the cursor by voice. Use 口述 for "what you dictate"; 听写 / 聽寫 only inside the feature name. There is no separate ja noun: ja says 口述. |
| context aware transcription | 按窗口改写听写 | 按視窗改寫聽寫 | ウィンドウに合わせた口述の書き直し | app/context `Context aware transcription` | Fixed feature name. The file header says to use it the same way everywhere. `Experimental` = 实验性 / 實驗性 / 試験的. |
| speech recognition | 语音识别 | 語音辨識 | 音声認識 | app/voice `Speech Recognition`; core/auxiliary `Recognition` | `Speech language` = 语音识别语言 / 語音辨識語言 / 音声認識の言語. |
| recogniser | 识别器 | 辨識器 | 認識 / 認識エンジン | core/auxiliary `Apple's recogniser from the next press.` Apple 识别器 / Apple 辨識器 / Apple の認識; core/app `Read the recogniser` 读取识别器 / 讀取辨識器 / 認識エンジンを取得 | ja: `Apple の認識` in user copy, 認識エンジン in technical rows. |
| synthesiser / text-to-speech | 语音合成器 | 語音合成器 | 音声合成 / 合成エンジン | core/auxiliary `Apple's synthesiser…` Apple 语音合成器 / Apple 語音合成器 / Apple の音声合成 | The app never says "TTS" in CJK copy except the model name `Qwen3-TTS`. |
| spoken replies | 语音回复 | 語音回覆 | 返信の読み上げ | core/auxiliary `Spoken replies` | |
| read aloud / announce | 朗读 / 播报 | 朗讀 / 播報 | 読み上げ | app/voice (朗读); fw/settings `Auto-announce` 自动播报 / 自動播報 / 自動読み上げ | |
| voice (a TTS voice) | 语音 | 語音 | 音声 | app/voice `Apple voice` Apple 语音; `Cloud voice, with your own key` 云端语音（自备密钥）/ 雲端語音（自備金鑰）/ クラウド音声（自分のキー） | |
| on this Mac / on this PC | 在此 Mac 上 / 在此电脑上 | 在此 Mac 上 / 在此電腦上 | Mac 上 / PC 上 | app/voice `Whisper, on this Mac` Whisper（在此 Mac 上）; `Whisper, on this PC` Whisper（在此电脑上）/ Whisper（在此電腦上）/ Whisper（PC 上） | On Windows the app swaps the whole word `Mac` for `PC` (deck-gui-core i18n.rs). |
| on-device | 本地运行 | 本機執行 | オンデバイス | core/platform `on-device` | |
| cloud | 云端 | 雲端 | クラウド | app/voice `Conol's cloud` Conol 云端 / Conol 雲端 / Conol クラウド | |
| signal path / voice path | 语音通路 | 語音路徑 | 音声経路 | app/voice `Signal path`; core/app `Test the voice path` | |
| chime | 提示音 | 提示音 | 通知音 | fw/settings `Chime` | |
| alerts | 提醒 | 提醒 | 通知 | fw/settings `Alerts` | |
| notices | 通知 | 通知 | 通知 | fw/overlays `NOTICES` | |

### Agents, integrations, sessions

| English | zh-Hans | zh-Hant | ja | Source | Note |
|---|---|---|---|---|---|
| agent / agents | 助手 | 助理 | エージェント | fw/agents `agent`; core/common `Agents` | **Agents are 助手 / 助理 / エージェント**, never 代理 / 智能体. (代理 appears only for macOS "launch agent": 启动代理 / 啟動代理程式, app/service.) |
| agent sessions | 助手会话 | 助理工作階段 | エージェント | app/voice `Agent sessions` | |
| agent tools | 助手工具 | 助理工具 | エージェントツール | app/integrations `Agent tools` | |
| session | 会话 | 工作階段 | セッション | app/integrations `1 session` 1 个会话 / 1 個工作階段 / 1 セッション | |
| chat | 聊天 | 聊天 | チャット | fw/agents `Chat` | `Conol chats` = Conol 聊天 / Conol 聊天 / Conol チャット. |
| thread | 对话 | 對話 | スレッド | app/integrations `1 thread` | |
| project | 项目 | 專案 | プロジェクト | fw/agents `project` | |
| workflow | 工作流 | 工作流程 | ワークフロー | fw/agents | |
| question / answer / reply | 提问 / 回复 / 回复 | 提問 / 回覆 / 回覆 | 質問 / 回答 / 返信 | fw/overlays `Question`; fw/agents `Answer`, `reply` | |
| test question | 测试提问 | 測試提問 | テスト質問 | app/kit `Send a Test Question` 发送测试提问 / 傳送測試提問 / テスト質問を送信 | |
| Integrations (page) | 集成 | 整合 | 連携 | core/common `Integrations` | `Pause integrations` = 暂停集成 / 暫停整合 / 連携を一時停止. |
| source (what puts sessions on the deck) | 来源 | 來源 | 接続元 | core/app `Source`, `Read the sources` 读取来源 / 讀取來源 / 接続元を取得 | |
| connected apps | 已连接的应用 | 已連線的應用程式 | 接続アプリ | app/integrations | |
| hook / hooks | 钩子 | 掛鉤 | フック | core/platform `hook` | Older hooks: 插件化之前的钩子 / 外掛化之前的掛鉤 / 従来のフック (app/service). |
| plugin | 插件 | 外掛 | プラグイン | app/kit `Claude Code plugin` Claude Code 插件 / Claude Code 外掛 / Claude Code プラグイン | |
| Claude Code, Codex, Gemini, MCP, Ghostty | unchanged | unchanged | unchanged | app/integrations | Latin, space on both sides in CJK. |
| permission | 权限 | 權限 | 権限 | core/platform `permission`; app/voice `macOS permissions` macOS 权限 / macOS 權限 / macOS の権限 | |
| approval | 批准 | 核准 | 承認 | core/platform `Service waiting for approval` 服务等待批准 / 服務等待核准 / サービスが承認待ち | |
| allow / deny | 允许 / 拒绝 | 允許 / 拒絕 | 許可 / 拒否 | core/app `Allow`, `Deny` | Permission prompts on the deck (approve a tool call). |
| granted / denied | 已授权 / 已拒绝 | 已授權 / 已拒絕 | 許可済み / 拒否済み | app/kit `Granted`; app/voice `Denied` | |
| Grok Bot | Grok Bot | Grok Bot | Grok Bot | app/integrations `Grok Bot is not installed on this Mac.` | Product name stays Latin. A bot in general = 机器人 / 機器人 / ボット; `Read the Grok bots` = 读取 Grok 机器人 (core/app). |
| routine / webhook | 例行任务 / Webhook | 例行任務 / Webhook | ルーティン / Webhook | app/integrations | |
| Usage (Claude/Codex usage) | Claude 用量 | Claude 用量 | Claude の使用量 | app/faces `Claude usage` | |

### Settings, models, keys and tokens

| English | zh-Hans | zh-Hant | ja | Source | Note |
|---|---|---|---|---|---|
| Settings | 设置 | 設定 | 設定 | core/common, fw/common | `Open Settings` = 打开设置 / 開啟設定 / 設定を開く (core/auxiliary). |
| config (technical) | 配置 | 設定 | 設定 | core/app `Read the config` 读取配置 / 讀取設定 / 設定を取得 | zh-Hans separates 配置 (config file) from 设置 (settings UI); zh-Hant and ja do not. |
| Preferences | 偏好设置 | 偏好設定 | 環境設定 | app/service | |
| provider | 服务提供商 / 服务商 | 服務供應商 / 服務商 | プロバイダー / プロバイダ | core/auxiliary `Provider` 服务提供商 / 服務供應商 / プロバイダー (voice: speech providers); app/context `Add provider` 添加服务商 / 新增服務商 / プロバイダを追加 (Model access) | **Inconsistent.** Voice/TTS rows use the long form; Model access and the deck's Usage face (`providers` 服务商 / 服務商 / サービス, fw/faces) use the short form. Recommend 服务商 / 服務商 / プロバイダ for model and AI providers (the newer page), 服务提供商 / 服務供應商 / プロバイダー only when quoting the Voice page. |
| model | 模型 | 模型 | モデル | fw/agents `Model`; app/context | `Whisper model` = Whisper 模型 / Whisper モデル. |
| model access | 模型接入 | 模型接入 | モデルへの接続 | app/context `Model access` | Page name. |
| rewriting model | 改写模型 | 改寫模型 | 書き直しのモデル | app/context | |
| API key | API 密钥 | API 金鑰 | API キー | app/context `API key` | Space between "API" and the CJK word. |
| key (secret) | 密钥 | 金鑰 | キー | app/voice `Add Key` 添加密钥 / 新增金鑰 / キーを追加; app/integrations `The routine's key` | **Two different "keys" in Chinese:** a secret is 密钥 / 金鑰, a deck key is 按键 / 按鍵. ja uses キー for both, so make context clear in ja (e.g. API キー, マクロパッドのキー). |
| macro key | “快捷键”页面的按键 | 「快捷鍵」頁面的按鍵 | マクロパッドのキー | core/app `Change the macro keys` 更改“快捷键”页面的按键 / 變更「快捷鍵」頁面的按鍵 / マクロパッドのキーを変更 | The app never translates "macro key" literally. |
| shortcut (what a macro key types) | 组合键 | 組合鍵 | ショートカット | app/faces `Lets the deck type shortcuts.` 允许设备发送组合键。/ 允許裝置傳送組合鍵。/ ショートカットの入力に必要です。 | |
| modifiers | 修饰键 | 修飾鍵 | 修飾キー | fw/overlays, app/faces | |
| label | 标签 | 標籤 | ラベル | fw/overlays `Label` | |
| token | 令牌 | 權杖 | トークン | core/auxiliary `Tokens` | Local API tokens. |
| Local API / port | 本地 API / 端口 | 本機 API / 連接埠 | ローカル API / ポート | app/service `Local API`; core/auxiliary `Port` | |
| Keychain / Credential Manager | 钥匙串 / 凭据管理器 | 鑰匙圈 / 認證管理員 | キーチェーン / 資格情報マネージャー | app/voice | |
| Accessibility (macOS permission) | 辅助功能 | 輔助使用 | アクセシビリティ | app/faces | |
| Login Items / Start at login | 登录项 / 登录时启动 | 登入項目 / 登入時啟動 | ログイン項目 / ログイン時に起動 | app/service, app/firstrun | `System Settings > General > Login Items` = “系统设置 > 通用 > 登录项” / 「系統設定 > 一般 > 登入項目」 / 「システム設定 > 一般 > ログイン項目」. |
| Demo data | 演示数据 | 展示資料 | デモデータ | fw/settings | |

### Link and connection

| English | zh-Hans | zh-Hant | ja | Source | Note |
|---|---|---|---|---|---|
| link (noun) | 连接 | 連線 | 接続 | fw/settings `Link`; app/deck `Link` | |
| linked / connected | 已连接 | 已連線 | 接続済み | core/platform `Linked`; core/common `Connected` | Both English words translate the same. |
| not linked / disconnected | 未连接 | 未連線 | 未接続 | core/platform `Not linked`; fw/settings `Disconnected` | |
| connect (an action, physically) | 连接 | 連接 | 接続 | app/firstrun `Connect`, `Connect your deck` 连接你的设备 / 連接你的裝置 / デバイスを接続 | zh-Hant: 連接 for plugging in / connecting by USB (`請透過 USB 連接裝置`), 連線 for the link state. ja: 接続 for both. |
| linked over Bluetooth | 已通过蓝牙连接 | 已透過藍牙連線 | Bluetooth で接続済み | core/platform | |
| linked by cable | 已通过数据线连接 | 已透過連接線連線 | ケーブルで接続済み | core/platform | |
| Bluetooth | 蓝牙 | 藍牙 | Bluetooth | app/kit | ja keeps Latin. `Bluetooth pairing` = 蓝牙配对 / 藍牙配對 / Bluetooth のペアリング (core/auxiliary). |
| cable | 数据线 | 連接線 | ケーブル | app/kit `Cable` | |
| USB / USB-C | USB / USB-C | USB / USB-C | USB / USB-C | core/auxiliary `Keep USB-C firmly connected` 请确保 USB-C 牢固连接 | Latin, spaced. `USB-C 数据线` / `USB-C 連接線` / `USB-C ケーブル` (app/firstrun). |
| offline | 离线 | 離線 | オフライン | fw/common | |

### The companion app's pages, menus and actions

| English | zh-Hans | zh-Hant | ja | Source | Note |
|---|---|---|---|---|---|
| Deck page | 设备 | 裝置 | デバイス | core/common `Deck` | Sidebar page names are the bare nouns. |
| Faces & Keys page | 页面与按键 | 頁面與按鍵 | ページとキー | app/kit `Faces & Keys` | |
| Service page | 服务 | 服務 | サービス | core/common `Service`; app/kit `Open Service` 打开“服务” / 開啟「服務」 / 「サービス」を開く | In a sentence the page name is quoted. |
| Voice page | 语音 | 語音 | 音声 | core/common `Voice` | |
| Integrations page | 集成 | 整合 | 連携 | core/common | |
| first run: welcome | — | — | — | not in the app | No "Welcome" string. First-run titles: `Looking for your deck` 正在寻找你的设备 / 正在尋找你的裝置 / デバイスを探しています; `What this app adds` 此应用将添加的内容 / 此應用程式將新增的內容 / このアプリが追加するもの (app/firstrun). |
| Check for Updates… | 检查更新… | 檢查更新… | アップデートを確認… | core/platform | Menu item. |
| update / updates | 更新 | 更新 | 更新 / アップデート | core/auxiliary `Updates`; app/service `The update did not install: {0}` アップデートをインストールできませんでした | ja: 更新 for labels and firmware; アップデート for the app update in menu/error text. Recommend 更新 on the website, except "Check for Updates…". |
| install / uninstall | 安装 / 卸载 | 安裝 / 解除安裝 | インストール / アンインストール | core/common `Install`, `Uninstall` | |
| installed | 已安装 | 已安裝 | インストール済み | app/kit | |
| download | 下载 | 下載 | ダウンロード | app/service `Download…` | |
| set up | 设置 | 設定 | 設定する | app/kit `Set Up…`; app/integrations `Set Up` | |
| setup (state) | 安装状态 | 安裝狀態 | セットアップ | app/service `Check my setup` 检查安装状态 / 檢查安裝狀態 / セットアップを確認 | |
| repair | 修复 | 修復 | 修復 | core/platform `Repair` | |
| Footprint | 安装内容 | 安裝內容 | インストール内容 | app/service `Footprint` | "What this app has added to your Mac". |
| support file | 支持文件 | 支援檔案 | サポートファイル | app/service `Export Support File` 导出支持文件 / 匯出支援檔案 / サポートファイルを書き出す | |
| log / logs | 日志 | 記錄 | ログ | app/kit `Open Log`; app/service `Logs` | |
| status item / menu bar | 菜单栏图标 | 選單列圖像 | メニューバー項目 | app/frame | |
| Quit OpenWalkieTalkie | 退出 OpenWalkieTalkie | 結束 OpenWalkieTalkie | OpenWalkieTalkie を終了 | core/platform | |
| Copy (menu) / copy (action) | 拷贝 / 复制 | 拷貝 / 複製 | コピー | app/frame `Copy` (macOS menu words); core/common `Copied` 已复制 / 已複製 | 拷贝 only in the macOS Edit menu. Use 复制 / 複製 on the site. |
| terminal | 终端 | 終端機 | ターミナル / 端末 | app/kit `run this command in a terminal` 终端 / 終端機 / ターミナル; fw/faces `terminal` 端末 | ja: ターミナル in the app, 端末 on the deck. Recommend ターミナル. |
| command / command line | 命令 / 命令行 | 指令 / 命令列 | コマンド / コマンドライン | app/kit `Copy Command` 复制命令 / 複製指令; app/voice `Set up from the command line.` | zh-Hant also writes 執行命令 in app/integrations; prefer 指令. |
| Join the waitlist | 加入候补名单 | 加入候補名單 | ウェイトリストに登録 | app/firstrun | **In the app.** Use on the site. |
| Build your own | 自己动手制作 | 自己動手製作 | 自作する | app/firstrun | |
| Try it in the browser | 在浏览器中试用 | 在瀏覽器中試用 | ブラウザーで試す | app/firstrun | |
| preview (image) | 预览 | 預覽 | プレビュー | fw/common `Preview unavailable` 无法预览 / 無法預覽 / プレビュー不可 | For a "preview release", see section 3. |
| version / build | 版本 | 版本 | バージョン / ビルド | core/auxiliary `automatic updates are unavailable in this build` 此版本不支持自动更新 / 此版本不支援自動更新 / このビルドでは自動更新を利用できません | zh uses 版本 for both. |

## 2. Style observed in the app

**Quote marks**

- zh-Hans: curly double quotes “…”, for page names, button names and quoted
  text: `打开“服务”`, `“快捷键”页面`, `点击“写入这些变更”前`.
- zh-Hant: corner brackets 「…」: `開啟「服務」`, `「快捷鍵」頁面`.
- ja: corner brackets 「…」: `「サービス」を開く`, `「エージェント」と「設定」`.
- Product names given as words stay in the same marks: `“Codex with OpenWalkieTalkie”` / `「Codex with OpenWalkieTalkie」`.

**Punctuation**

- Chinese uses full-width punctuation: ，。：；？！（）. Example: `{0}：{1}`,
  `Whisper（在此 Mac 上）`, `端口必须是 1 至 65535 的数字。`.
- ja uses 、 and 。, full-width （）, and a full-width ＋ in `音楽＋効果音`.
- ja colons are inconsistent. Full-width `：` is used in core/app (`{0}：{1}`),
  app/context and app/integrations. Half-width `: ` (colon plus space) is used
  in core/auxiliary, app/deck, app/faces and app/firstrun (`最終接続: {0}`,
  `左上: 本体ボタン`). **Recommend full-width `：` for the website.**
- Ellipsis: always the single character `…` (U+2026), with no space before it,
  in every language: `检查更新…`, `正在等待设备…`, `削除…`. The Chinese text
  does not use the doubled `……`.
- Dashes: an em dash with spaces, ` — `, is kept in zh and ja
  (`完成 — 已复制`, `引导程序更新 — 必须保持供电`).
- Middle dot ` · ` with spaces separates short facts in every language
  (`已连接 · {0}%`).
- Arrows and path separators stay as in English: `“服务 → 更新”`, `设置 › 显示 › 教程`, `系统设置 > 通用 > 登录项`.

**Spacing between CJK and Latin text or numbers**

- zh-Hans and zh-Hant: put one half-width space between CJK and Latin words or
  numbers: `API 密钥`, `Claude Code 插件`, `通过 USB 连接`, `2 秒`, `{0} 个会话`,
  `最多 12 个字母`. There is no space next to full-width punctuation:
  `Whisper（在此 Mac 上）`.
- ja in the companion app usually does the same: `API キー`, `Bluetooth で接続済み`,
  `{0} 秒前`, `あと 1 ステップです`. The deck firmware often leaves the space
  out: `Macアプリ`, `PTT を2秒押すと`, `CANCEL を3秒長押し`, `1024未満`.
  **Recommend the spaced form (the companion app's) for the website.**

**Latin names stay Latin**

OpenWalkieTalkie, deckd, Omarchy, Claude, Claude Code, Codex, Gemini, MCP,
Ghostty, Grok Bot, Whisper, Kokoro, Qwen3-TTS, OpenAI, ElevenLabs, Anthropic,
Conol, macOS, Windows, Mac, PC, USB, USB-C, HTTP, CLI, Webhook, and the key
names PTT, SHIFT, ENTER and CANCEL are never translated or transliterated.
Bluetooth is translated in Chinese (蓝牙 / 藍牙) but stays Latin in ja.

**Japanese register**

- Sentences use です / ます: `キーを保存しました。`, `ケーブルを外しているとき、デバイスのマイクとスピーカーに使います。`,
  `先にデバイスを接続してください。`.
- Labels, buttons and status lines are plain nouns or short verb phrases,
  without です: `接続済み`, `設定を開く`, `確認が必要`, `音声をテスト`.
- On the deck, hints are clipped to fit: `閉じて画像を再タップ`, `PTT でラベル入力`.
- Long vowel marks are inconsistent: `プロバイダー` (voice) vs `プロバイダ`
  (context), `コンピューター` (integrations) vs `コンピュータ` (context), and
  `フォルダ` everywhere. ja `ブラウザー`, `メモリー`, `プロセッサー` take the mark.
  **Recommend the forms with ー (プロバイダー, コンピューター), except
  フォルダ, as the app writes it.** For "computer" in plain prose the deck and
  the app say `パソコン`.

**Chinese register**

- zh-Hans and zh-Hant address the reader as 你 (not 您), except one row:
  `接下来请您操作` (core/auxiliary). Use 你.
- Requests start with 请 / 請: `请先连接设备。` / `請先連線裝置。`.

**zh-Hans vs zh-Hant vocabulary in this app** (all pairs below are in the files)

| zh-Hans | zh-Hant | Example source |
|---|---|---|
| 设备 | 裝置 | core/common `Deck` |
| 设置 | 設定 | core/common `Settings` |
| 配置 | 設定 | core/app `Read the config` |
| 固件 | 韌體 | fw/settings `Firmware` |
| 连接 (link) | 連線 | core/platform `Linked` |
| 连接 (plug in) | 連接 | app/firstrun `Connect` |
| 数据线 | 連接線 | app/kit `Cable` |
| 应用 | 應用程式 | app/service |
| 后台服务 | 背景服務 | app/kit |
| 程序 (引导程序, 辅助程序, 更新程序) | 程式 (開機載入程式, 輔助程式, 更新程式) | core/auxiliary, app/kit |
| 进程 | 程序 | core/platform `another process` 其他进程 / 其他程序 |
| 文件 | 檔案 | core/auxiliary `file(s) written` |
| 文件夹 | 資料夾 | app/firstrun |
| 配置文件 | 設定檔 | fw/overlays |
| 插件 | 外掛 | app/kit |
| 钩子 | 掛鉤 | core/platform |
| 令牌 | 權杖 | core/auxiliary |
| 密钥 | 金鑰 | app/context |
| 端口 | 連接埠 | core/auxiliary |
| 本地 | 本機 | app/service `Local API` |
| 会话 | 工作階段 | app/integrations |
| 助手 | 助理 | core/common `Agents` |
| 项目 | 專案 | fw/agents `project` |
| 集成 | 整合 | core/common `Integrations` |
| 日志 | 記錄 | app/kit `Open Log` |
| 支持 (support) | 支援 | core/auxiliary, app/service |
| 导出 | 匯出 | core/app |
| 窗口 | 視窗 | fw/faces |
| 屏幕 | 螢幕 | app/deck |
| 菜单 / 菜单栏 | 選單 / 選單列 | fw/agents, app/frame |
| 默认 | 預設 | core/platform |
| 信息 | 資訊 | app/deck |
| 消息 | 訊息 | fw/agents |
| 数据 | 資料 | fw/faces |
| 硬件 / 软件 | 硬體 / 軟體 | core/auxiliary, core/platform |
| 主板 | 主機板 | core/auxiliary |
| 识别 | 辨識 | core/auxiliary |
| 服务器 / 地址 | 伺服器 / 位址 | app/context |
| 用户 | 使用者 | core/auxiliary |
| 保存 | 儲存 | fw/overlays `Save` |
| 粘贴 | 貼上 | core/auxiliary |
| 添加 | 新增 | app/firstrun |
| 创建 | 建立 | core/auxiliary |
| 运行 | 執行 | core/platform `running` |
| 打开 | 開啟 | core/platform |
| 卸载 | 解除安裝 | core/common |
| 退出 (quit) | 結束 | core/platform |
| 命令行 | 命令列 | app/voice |
| 终端 | 終端機 | app/kit, fw/faces |
| 搜索 | 搜尋 | app/context |
| 刷新 | 重新整理 | app/context, fw/faces |
| 模板 | 範本 | fw/overlays |
| 列表 | 清單 (also 列表) | app/faces `Face strip` |
| 演示 | 展示 | fw/settings |
| 教程 | 教學 | fw/overlays |
| 内存 / 存储 | 記憶體 / 儲存空間 | fw/faces |
| 滚动 | 捲動 | fw/common |
| 触摸 | 觸控 | app/faces |
| 跳过 | 略過 | fw/overlays |
| 推荐 | 建議 | fw/agents |
| 秒表 | 碼錶 | fw/faces |
| 废纸篓 | 垃圾桶 | app/service |
| 钥匙串 | 鑰匙圈 | app/voice |
| 云端 | 雲端 | app/voice |
| 磁盘 | 磁碟 | app/integrations |
| 二进制 | 二進位 | core/platform |
| 组件 | 元件 | core/platform |
| 脚本 | 指令碼 | core/auxiliary |
| 批准 | 核准 | core/platform |
| 实时 | 即時 | fw/faces |
| 标签页 | 分頁 | app/integrations |

## 3. Terms not in the app (website only)

**These are recommendations, not app strings.** They follow the style above:
spaces around Latin words, “” in zh-Hans, 「」 in zh-Hant and ja, Taiwan
vocabulary in zh-Hant, and です / ます in ja sentences. Note that "Join the
waitlist", "Build your own", "Try it in the browser" and "Download" are in the
app; use the main table for them.

| English | zh-Hans | zh-Hant | ja | Reasoning |
|---|---|---|---|---|
| Waitlist (noun) | 候补名单 | 候補名單 | ウェイトリスト | From the app's `Join the waitlist`. |
| You're on the list | 你已加入候补名单 | 你已加入候補名單 | ウェイトリストに登録しました | |
| Home / home page | 首页 | 首頁 | ホーム | The deck uses 首页 / 首頁 / ホーム in `Idle home`. |
| Features | 功能 | 功能 | 機能 | |
| Blog | 博客 | 部落格 | ブログ | zh-Hant Taiwan usage: 部落格. |
| radio (the product metaphor: "handheld radio", "the yellow pixel radio") | 对讲机 | 對講機 | 無線機 | Decided after the first translations disagreed: the product is a walkie-talkie, so 对讲机 / 對講機, not 无线电 / 無線電 (which reads as "radio broadcasting"). ja 無線機 (トランシーバー is also understood; keep one). |
| Back to the blog | 返回博客 | 返回部落格 | ブログに戻る | The app's 返回 / 返回 / 戻る. |
| Feed (RSS) | 订阅源 | 訂閱來源 | フィード | |
| Docs / documentation | 文档 | 說明文件 | ドキュメント | zh-Hant 文件 means "document" in Taiwan; 說明文件 is clearer. |
| Field manual | 使用手册 | 使用手冊 | 取扱説明書 | Matches the app's 使用教程 / 使用教學 register. |
| Guide (install guide) | 指南 | 指南 | ガイド | "Windows install guide" = Windows 安装指南 / Windows 安裝指南 / Windows インストールガイド. |
| Release notes | 更新说明 | 版本資訊 | リリースノート | |
| Release (a GitHub release) | 版本 / 发布版本 | 版本 / 發行版本 | リリース | |
| Preview (preview build) | 预览版 | 預覽版 | プレビュー版 | The app's 预览 / 預覽 / プレビュー plus 版. "Windows, as a preview" = Windows 预览版 / Windows 預覽版 / Windows（プレビュー版）. |
| Download (noun, button) | 下载 | 下載 | ダウンロード | From the app's `Download…`. |
| Privacy Policy | 隐私政策 | 隱私權政策 | プライバシーポリシー | Taiwan usage: 隱私權政策. |
| Privacy notice | 隐私声明 | 隱私權聲明 | プライバシーに関するお知らせ | |
| Cookie information | Cookie 说明 | Cookie 說明 | Cookie について | Keep "Cookie" Latin with a space. |
| Skip to content | 跳至正文 | 跳至主要內容 | 本文へスキップ | |
| Website / site | 网站 | 網站 | ウェブサイト / サイト | |
| Hardware | 硬件 | 硬體 | ハードウェア | Same words as the app's bootloader warning. |
| Software | 软件 | 軟體 | ソフトウェア | The app has ソフト; use ソフトウェア in headings. |
| Open source | 开源 | 開源 | オープンソース | |
| Enclosure / case | 外壳 | 外殼 | 本体ケース | The app's `Case` 外壳 / 外殼. ja: the app only has 本体カラー, so 本体ケース is coined. |
| Colourway | 外壳颜色 / 配色 | 外殼顏色 / 配色 | 本体カラー | Use the app's `Case colour` when it means the case. |
| PCB / circuit board | 电路板（PCB） | 電路板（PCB） | 基板（PCB） | The app has 主板 / 主機板 / 基板 for "board". Use those when the text means the deck's board. |
| Schematic | 原理图 | 電路圖 | 回路図 | |
| Bill of materials (BOM) | 物料清单（BOM） | 物料清單（BOM） | 部品表（BOM） | |
| Blueprint (site section) | 设计图纸 | 設計藍圖 | 設計図 | A section name: translate it the same way everywhere. |
| The Pile (site section) | The Pile | The Pile | The Pile | A proper name: leave in Latin, or add a gloss on first use. |
| Try It (nav) | 试用 | 試用 | 試す | From the app's `Try it` 试用 / 試用 / 試す (core/auxiliary). |
| Example | 示例 | 範例 | サンプル / 例 | The app's `Speak a Sample` 朗读示例 / 朗讀範例 / サンプルを再生. |
| Open an issue (GitHub) | 提交 Issue | 提交 Issue | Issue を作成 | |
| View on GitHub | 在 GitHub 上查看 | 在 GitHub 上檢視 | GitHub で見る | zh-Hant 檢視 matches the app's `Review…` 檢視…. |
| Theme (Omarchy theme) | 主题 | 主題 | テーマ | |
| Screen lock | 锁屏 | 螢幕鎖定 | 画面ロック | The app only has `Locked` 已锁定 / 已鎖定 / ロック中. |
| Factory image | 出厂固件 | 原廠韌體 | 出荷時ファームウェア | Built from the app's 出厂 / 原廠 (factory reset) and 固件 / 韌體. |
| Dictation at the cursor | 在光标处听写 | 在游標處聽寫 | カーソル位置に口述入力 | 听写 / 聽寫 / 口述 as in the app. Taiwan 游標. |
| Text-to-speech (TTS) | 语音合成 | 語音合成 | 音声合成 | The app's synthesiser words. |
| Speech-to-text | 语音识别 | 語音辨識 | 音声認識 | The app's recognition words. |
| Email (waitlist email) | 邮箱 / 电子邮件 | 電子郵件 | メールアドレス | |
| Subscribe / sign up | 订阅 / 注册 | 訂閱 / 註冊 | 登録 | |

## Approved Chinese website wording, 2026-11-24

The reviewed website preview deliberately uses **等候名单**, **加入等候名单**
and **宏键盘** in its explanatory copy. These user-approved site choices take
precedence over the corresponding website recommendations above. Literal
localized app labels remain as listed in this glossary. The illustrated manual
uses the English labels visible in its screenshots. See TRANSLATING.md for the
review scope and status. The same product-context review also covers Japanese
and Traditional Chinese (Taiwan): website waitlist copy uses 等候名單 / 加入等候名單
in Traditional Chinese, while literal app labels keep the wording shown in the app.
Translate functional pages and computer/service references by their actual role,
not by the isolated English words “face” or “host”.
