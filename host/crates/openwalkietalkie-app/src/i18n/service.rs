//! The words of the Service page, Footprint and Local API, each as
//! `(English, 简体中文, 繁體中文, 日本語)`. English is the key.
//!
//! The rows are the service sheet's (`service.tsv` of the string sheets),
//! copied as written and in its order, with the support file's second note
//! beside the first and the app row's line for a newer app beside the line
//! it replaces; then the two this page added, and the sentences of the
//! service's own firmware status, which the review dialog shows. A word
//! the page shares with another page (Background service, Details, Remove…,
//! Delete…, the old-helper lines, Not checked…) is in `crate::i18n::kit`, and
//! one the built-in tables already ship (Service, Restart, Stop, Start,
//! Updates, the port, token and firmware sentences…) is not repeated: one key
//! has one set of words wherever it is.
use deck_gui_core::i18n::Entry;

pub const STRINGS: &[Entry] = &[
    // The page's head, and the background service: running or not, Stop…
    // with its question, and Start at login with its own.
    (
        "The background service, updates, and what this app has added to your Mac.",
        "后台服务、更新，以及此应用在你的 Mac 上添加的内容。",
        "背景服務、更新，以及此應用程式在你的 Mac 上新增的內容。",
        "バックグラウンドサービス、更新、このアプリが Mac に追加したもの。",
    ),
    ("The service is running", "服务运行中", "服務執行中", "サービス実行中"),
    (
        "It keeps the deck linked.",
        "它负责保持设备连接。",
        "它負責保持裝置連線。",
        "デバイスとの接続を保ちます。",
    ),
    ("Stop…", "停止…", "停止…", "停止…"),
    (
        "Stop the background service?",
        "要停止后台服务吗？",
        "要停止背景服務嗎？",
        "バックグラウンドサービスを停止しますか？",
    ),
    (
        "The deck stops working until you start it again.",
        "再次启动服务之前，设备将停止工作。",
        "再次啟動服務之前，裝置將停止運作。",
        "再び開始するまで、デバイスは動作しません。",
    ),
    ("Start at login", "登录时启动", "登入時啟動", "ログイン時に起動"),
    (
        "Turn off Start at login?",
        "要关闭“登录时启动”吗？",
        "要關閉「登入時啟動」嗎？",
        "「ログイン時に起動」をオフにしますか？",
    ),
    (
        "The service stops now and does not start when you log in.",
        "服务会立即停止，登录时也不会启动。",
        "服務會立即停止，登入時也不會啟動。",
        "サービスはすぐに停止し、ログイン時にも起動しません。",
    ),
    ("Turn Off", "关闭", "關閉", "オフにする"),
    (
        "Only in the installed app.",
        "仅限已安装的应用。",
        "僅限已安裝的應用程式。",
        "インストールしたアプリでのみ使えます。",
    ),
    // The updates: the app's, and the deck's firmware.
    (
        "OpenWalkieTalkie {0}",
        "OpenWalkieTalkie {0}",
        "OpenWalkieTalkie {0}",
        "OpenWalkieTalkie {0}",
    ),
    (
        "The app and the service update together.",
        "应用和服务会一起更新。",
        "應用程式和服務會一起更新。",
        "アプリとサービスは一緒に更新されます。",
    ),
    // Windows: the app's button once a newer preview is out, which opens
    // the page its zip is downloaded from.
    ("Download…", "下载…", "下載…", "ダウンロード…"),
    // In its place once the service's look found a newer app (R8), in the
    // words of the shipped "Deck firmware {0} is available".
    (
        "OpenWalkieTalkie {0} is available.",
        "OpenWalkieTalkie {0} 现已可用。",
        "OpenWalkieTalkie {0} 現已可用。",
        "OpenWalkieTalkie {0} が利用可能です。",
    ),
    ("Check", "检查更新", "檢查", "確認"),
    ("Deck firmware {0}", "设备固件 {0}", "裝置韌體 {0}", "デバイスのファームウェア {0}"),
    // The sheet's own line, left out of the redesign until the service
    // asked for the press.
    (
        "By cable, after a press on the deck.",
        "通过数据线更新，需先在设备上按键确认。",
        "透過連接線更新，需先在裝置上按鍵確認。",
        "ケーブル接続で、本体のボタンを押して開始。",
    ),
    ("Review…", "查看…", "檢視…", "内容を確認…"),
    // The local API: its row, its switch's questions, and its page.
    ("Local API", "本地 API", "本機 API", "ローカル API"),
    ("HTTP port and tokens", "HTTP 端口和令牌", "HTTP 連接埠與權杖", "HTTP ポートとトークン"),
    (
        "On. Port {0}. Local tools can use it with a token.",
        "已开启。端口 {0}。本地工具可凭令牌使用。",
        "已開啟。連接埠 {0}。本機工具可憑權杖使用。",
        "オン。ポート {0}。トークンで利用できます。",
    ),
    (
        "Off. Nothing shipped with the deck uses them.",
        "已关闭。设备附带的软件都不使用它们。",
        "已關閉。裝置隨附的軟體都不會用到。",
        "オフ。デバイス付属のソフトでは使いません。",
    ),
    (
        "Turn the local API off?",
        "要关闭本地 API 吗？",
        "要關閉本機 API 嗎？",
        "ローカル API をオフにしますか？",
    ),
    (
        "Turn the local API on?",
        "要开启本地 API 吗？",
        "要開啟本機 API 嗎？",
        "ローカル API をオンにしますか？",
    ),
    ("The service restarts.", "服务将重新启动。", "服務將重新啟動。", "サービスが再起動します。"),
    ("Turn On", "开启", "開啟", "オンにする"),
    // The question to turn it off, while Claude Code's hooks from before the
    // plugin still post to it; its button is the shipped "Repair". The
    // shipped words for those hooks (the Integrations page's Repair hint):
    // 插件化之前的钩子, 外掛化之前的掛鉤, 従来のフック.
    (
        "Claude Code's older hooks still use the local API. Repair them first, or Claude Code's questions stay in the terminal.",
        "Claude Code 插件化之前的钩子仍在使用本地 API。请先“修复”，否则 Claude Code 的问题只会留在终端中。",
        "Claude Code 外掛化之前的掛鉤仍在使用本機 API。請先「修復」，否則 Claude Code 的問題只會留在終端機中。",
        "Claude Code の従来のフックがまだローカル API を使っています。先に「修復」してください。修復しないと、Claude Code の質問はターミナルにしか表示されません。",
    ),
    // Check my setup, and the support file.
    ("Check my setup", "检查安装状态", "檢查安裝狀態", "セットアップを確認"),
    ("The deck is linked", "设备已连接", "裝置已連線", "デバイスは接続済み"),
    ("The deck is not linked", "设备未连接", "裝置未連線", "デバイスは未接続"),
    (
        "The service runs from this copy of the app",
        "服务从当前应用副本运行",
        "服務由此應用程式副本執行",
        "サービスはこのアプリ自身から実行中",
    ),
    (
        "The service runs from another copy of the app",
        "服务从另一个应用副本运行",
        "服務由另一個應用程式副本執行",
        "サービスは別のアプリのコピーから実行中",
    ),
    (
        "Quit the other copy, then restart the service.",
        "请退出另一个副本，然后重新启动服务。",
        "請結束另一個副本，然後重新啟動服務。",
        "もう一方を終了し、サービスを再起動してください。",
    ),
    (
        "Claude Code plugin is current",
        "Claude Code 插件已是最新版本",
        "Claude Code 外掛為最新版本",
        "Claude Code プラグインは最新",
    ),
    (
        "Claude Code plugin needs repair",
        "Claude Code 插件需要修复",
        "Claude Code 外掛需要修復",
        "Claude Code プラグインの修復が必要",
    ),
    (
        "Claude Code plugin is not set up",
        "Claude Code 插件未设置",
        "Claude Code 外掛未設定",
        "Claude Code プラグインは未設定",
    ),
    ("Codex is reachable", "可以连接到 Codex", "可以連線到 Codex", "Codex に接続可能"),
    (
        "No leftover launch agent",
        "没有残留的启动代理",
        "沒有殘留的啟動代理程式",
        "古い launchd エージェントなし",
    ),
    (
        "An old launch agent is still installed",
        "仍有残留的启动代理",
        "仍有殘留的啟動代理程式",
        "古い launchd エージェントあり",
    ),
    (
        "Remove it with Uninstall, or by hand in ~/Library/LaunchAgents.",
        "请用“卸载”移除，\n或在 ~/Library/LaunchAgents 中手动删除。",
        "請用「解除安裝」移除，\n或在 ~/Library/LaunchAgents 中手動刪除。",
        "「アンインストール」で削除するか、\n~/Library/LaunchAgents から手動で削除します。",
    ),
    ("Bluetooth is allowed", "已允许使用蓝牙", "已允許使用藍牙", "Bluetooth は許可済み"),
    ("Bluetooth is not allowed", "未允许使用蓝牙", "未允許使用藍牙", "Bluetooth は未許可"),
    ("Run Again", "重新检查", "重新檢查", "もう一度確認"),
    ("Export Support File", "导出支持文件", "匯出支援檔案", "サポートファイルを書き出す"),
    (
        "Saved to the Desktop: {0}",
        "已保存到桌面：{0}",
        "已儲存到桌面：{0}",
        "デスクトップに保存しました: {0}",
    ),
    // Whether the service's redaction leaves the text said in sessions out
    // of the file: it does while content logging is off, its default (the
    // sheet's row, the board's line), and keeps it while it is on (added by
    // the page).
    (
        "The file is saved to the Desktop. Session text is left out.",
        "文件会保存到桌面，不包含会话文本。",
        "檔案會儲存到桌面，不包含工作階段文字。",
        "デスクトップに保存され、セッションの文章は含みません。",
    ),
    (
        "The file is saved to the Desktop. Session text is kept in it.",
        "文件会保存到桌面，包含会话文本。",
        "檔案會儲存到桌面，包含工作階段文字。",
        "デスクトップに保存され、セッションの文章も含まれます。",
    ),
    // The footprint: the Service page's line, and its page's rows, sizes and
    // questions.
    ("Footprint", "安装内容", "安裝內容", "インストール内容"),
    (
        "What this app has added to your Mac",
        "此应用在你的 Mac 上添加的内容",
        "此應用程式在你的 Mac 上新增的內容",
        "このアプリが Mac に追加したもの",
    ),
    (
        "{0} items, {1}. Most can be removed one at a time.",
        "共 {0} 项，{1}。大多数可以逐项移除。",
        "共 {0} 項，{1}。大多數可以逐項移除。",
        "{0} 項目、{1}。ほとんどは個別に削除できます。",
    ),
    ("Show All", "显示全部", "顯示全部", "すべて表示"),
    (
        "What this app has added to your Mac, and what Uninstall does with each.",
        "此应用在你的 Mac 上添加的内容，以及“卸载”对每一项的处理方式。",
        "此應用程式在你的 Mac 上新增的內容，以及「解除安裝」對每一項的處理方式。",
        "このアプリが Mac に追加したものと、「アンインストール」でのそれぞれの扱い。",
    ),
    (
        "deckd, under Login Items in System Settings",
        "deckd，位于“系统设置”的“登录项”中",
        "deckd，位於「系統設定」的「登入項目」中",
        "deckd（「システム設定」の「ログイン項目」内）",
    ),
    (
        "deckd, under Startup apps in Settings",
        "deckd，位于“设置”的“启动应用”中",
        "deckd，位於「設定」的「啟動應用程式」中",
        "deckd（「設定」の「スタートアップ アプリ」内）",
    ),
    // Japanese: the 104 px action cell holds eight full-width characters,
    // and アンインストール時 is nine; 削除対象 pairs with 保持 (Kept), and
    // the page's head already says these are what Uninstall does.
    ("With Uninstall", "随卸载移除", "隨解除安裝移除", "削除対象"),
    ("{0} hooks", "{0} 个钩子", "{0} 個掛鉤", "{0} フック"),
    ("1 hook", "1 个钩子", "1 個掛鉤", "1 フック"),
    ("Leftover Codex setup", "残留的 Codex 配置", "殘留的 Codex 設定", "残っている Codex 設定"),
    ("{0} entries", "{0} 个条目", "{0} 個項目", "{0} 件"),
    ("1 entry", "1 个条目", "1 個項目", "1 件"),
    ("Whisper model", "Whisper 模型", "Whisper 模型", "Whisper モデル"),
    (
        "Delete the Whisper model?",
        "要删除 Whisper 模型吗？",
        "要刪除 Whisper 模型嗎？",
        "Whisper モデルを削除しますか？",
    ),
    (
        "It is downloaded again when you choose Whisper.",
        "再次选择 Whisper 时会重新下载。",
        "再次選擇 Whisper 時會重新下載。",
        "Whisper を選ぶと再びダウンロードされます。",
    ),
    ("Delete", "删除", "刪除", "削除"),
    ("Kokoro voice", "Kokoro 语音", "Kokoro 語音", "Kokoro 音声"),
    (
        "Delete the Kokoro voice?",
        "要删除 Kokoro 语音吗？",
        "要刪除 Kokoro 語音嗎？",
        "Kokoro 音声を削除しますか？",
    ),
    (
        "It is downloaded again when you choose Kokoro.",
        "再次选择 Kokoro 时会重新下载。",
        "再次選擇 Kokoro 時會重新下載。",
        "Kokoro を選ぶと再びダウンロードされます。",
    ),
    ("Firmware downloads", "已下载的固件", "已下載的韌體", "保存済みファームウェア"),
    (
        "Delete the firmware downloads?",
        "要删除已下载的固件吗？",
        "要刪除已下載的韌體嗎？",
        "保存済みファームウェアを削除しますか？",
    ),
    (
        "Bootloader backups are kept.",
        "引导程序备份会保留。",
        "開機載入程式備份會保留。",
        "ブートローダーのバックアップは保持されます。",
    ),
    ("Settings and backups", "设置和备份", "設定與備份", "設定とバックアップ"),
    ("Preferences", "偏好设置", "偏好設定", "環境設定"),
    // The Keychain item that holds the cloud voice's key, in macOS's own
    // word for the Keychain in each language (as on the Usage panel).
    ("Keychain item", "钥匙串项目", "鑰匙圈項目", "キーチェーン項目"),
    // Windows: the same key in Credential Manager, in Windows' own word for
    // it in each language.
    (
        "Credential Manager entry",
        "凭据管理器条目",
        "認證管理員項目",
        "資格情報マネージャーの項目",
    ),
    (
        "Your cloud voice key",
        "你的云端语音密钥",
        "你的雲端語音金鑰",
        "お使いのクラウド音声のキー",
    ),
    ("Logs", "日志", "記錄", "ログ"),
    ("Kept", "保留", "保留", "保持"),
    // Uninstall: the page's note, its question and how it went.
    (
        "Uninstall removes all of these except the logs and your macOS permissions,\nand puts your CLI settings back. Move the app to the Trash yourself.",
        "“卸载”会移除以上所有内容（日志和你的 macOS 权限除外），\n并还原你的命令行工具设置。请自行将应用移到废纸篓。",
        "「解除安裝」會移除以上所有內容（記錄和你的 macOS 權限除外），\n並還原你的命令列工具設定。請自行將應用程式丟到垃圾桶。",
        "「アンインストール」は、ログと macOS の権限を除くこれらすべてを削除し、\nCLI の設定を元に戻します。アプリはご自身でゴミ箱に入れてください。",
    ),
    // Windows: the same, without macOS's permissions or its Trash.
    (
        "Uninstall removes all of these except the logs,\nand puts your CLI settings back. Delete the app's folder yourself.",
        "“卸载”会移除以上所有内容（日志除外），\n并还原你的命令行工具设置。请自行删除应用所在的文件夹。",
        "「解除安裝」會移除以上所有內容（記錄除外），\n並還原你的命令列工具設定。請自行刪除應用程式所在的資料夾。",
        "「アンインストール」は、ログを除くこれらすべてを削除し、\nCLI の設定を元に戻します。アプリのフォルダはご自身で削除してください。",
    ),
    (
        "Removes everything in this list except the logs, and puts your CLI settings back. Then delete the app's folder.",
        "将移除此列表中除日志以外的所有内容，\n并还原你的命令行工具设置。之后请删除应用所在的文件夹。",
        "將移除此清單中除記錄以外的所有內容，\n並還原你的命令列工具設定。之後請刪除應用程式所在的資料夾。",
        "ログを除き、この一覧のすべてを削除し、\nCLI の設定を元に戻します。\nその後、アプリのフォルダを削除してください。",
    ),
    (
        "OpenWalkieTalkie is uninstalled. Delete the app's folder.",
        "OpenWalkieTalkie 已卸载。请删除应用所在的文件夹。",
        "OpenWalkieTalkie 已解除安裝。請刪除應用程式所在的資料夾。",
        "OpenWalkieTalkie をアンインストールしました。アプリのフォルダを削除してください。",
    ),
    ("Uninstall…", "卸载…", "解除安裝…", "アンインストール…"),
    (
        "Uninstall OpenWalkieTalkie?",
        "要卸载 OpenWalkieTalkie 吗？",
        "要解除安裝 OpenWalkieTalkie 嗎？",
        "OpenWalkieTalkie をアンインストールしますか？",
    ),
    (
        "Removes everything in this list except the logs and your macOS permissions, and puts your CLI settings back. Then move the app to the Trash.",
        "将移除此列表中除日志和你的 macOS 权限以外的所有内容，\n并还原你的命令行工具设置。之后请将应用移到废纸篓。",
        "將移除此清單中除記錄和你的 macOS 權限以外的所有內容，\n並還原你的命令列工具設定。之後請將應用程式丟到垃圾桶。",
        "ログと macOS の権限を除き、この一覧のすべてを削除し、\nCLI の設定を元に戻します。\nその後、アプリをゴミ箱に入れてください。",
    ),
    (
        "OpenWalkieTalkie is uninstalled. Move the app to the Trash.",
        "OpenWalkieTalkie 已卸载。请将应用移到废纸篓。",
        "OpenWalkieTalkie 已解除安裝。請將應用程式丟到垃圾桶。",
        "OpenWalkieTalkie をアンインストールしました。アプリをゴミ箱に入れてください。",
    ),
    (
        "Uninstall did not finish: {0}",
        "卸载未完成：{0}",
        "解除安裝未完成：{0}",
        "アンインストールが完了しませんでした: {0}",
    ),
    // The Local API page.
    (
        "The HTTP port and the tokens local tools use.",
        "本地工具使用的 HTTP 端口和令牌。",
        "本機工具使用的 HTTP 連接埠與權杖。",
        "ローカルツールが使う HTTP ポートとトークン。",
    ),
    ("Change Port", "更改端口", "變更連接埠", "ポートを変更"),
    // Added by the page: Start at login while macOS waits for the owner, and
    // the question before a new port.
    (
        "Waiting for your approval in System Settings > General > Login Items.",
        "正在等待你在“系统设置 > 通用 > 登录项”中批准。",
        "正在等待你在「系統設定 > 一般 > 登入項目」中核准。",
        "「システム設定 > 一般 > ログイン項目」での承認を待っています。",
    ),
    (
        "Change the port to {0}?",
        "要将端口更改为 {0} 吗？",
        "要將連接埠變更為 {0} 嗎？",
        "ポートを {0} に変更しますか？",
    ),
    // The service's firmware status, which the review dialog shows: every
    // sentence `deckd` writes there (`firmware/mod.rs`, `firmware/migration.rs`),
    // looked up by its English (`updates::status_line`), not by a literal,
    // so the page's test reads them out of the service's sources.
    (
        "Connect your deck with USB to update its firmware.",
        "请通过 USB 连接设备以更新固件。",
        "請透過 USB 連接裝置以更新韌體。",
        "ファームウェアを更新するには、デバイスを USB 接続してください。",
    ),
    (
        "Checking for firmware updates…",
        "正在检查固件更新…",
        "正在檢查韌體更新…",
        "ファームウェアの更新を確認しています…",
    ),
    (
        "Firmware release checked. Installation requires your confirmation.",
        "已检查固件版本，安装前需要你确认。",
        "已檢查韌體版本，安裝前需要你確認。",
        "ファームウェアを確認しました。インストールには承認が必要です。",
    ),
    // The install waits for ENTER on a linked deck, and comes back with
    // this when the deck did not give it.
    (
        "Press ENTER on the deck to start the install.",
        "请在设备上按 ENTER 开始安装。",
        "請在裝置上按 ENTER 開始安裝。",
        "デバイスで ENTER を押して、インストールを開始してください。",
    ),
    (
        "Not confirmed on the deck. Nothing was installed.",
        "未在设备上确认，未安装任何内容。",
        "未在裝置上確認，未安裝任何內容。",
        "デバイスで承認されませんでした。何もインストールしていません。",
    ),
    // After ENTER, while someone is still talking (the install pauses the
    // deck's link, so it waits for the voice to go quiet).
    (
        "Starting the install when the deck stops talking…",
        "设备上的语音结束后将开始安装…",
        "裝置上的語音結束後將開始安裝…",
        "デバイスの音声が終わったら、インストールを開始します…",
    ),
    // Why an install is refused for a deck that is not in recovery and has
    // not linked: it cannot be asked for the press yet (`firmware.install`'s
    // reason, shown as it comes back).
    (
        "Wait for the deck to link, then install again.",
        "请等待设备连接，然后重新安装。",
        "請等待裝置連線，然後重新安裝。",
        "デバイスが接続されるのを待ってから、もう一度インストールしてください。",
    ),
    (
        "Downloading and verifying firmware…",
        "正在下载并验证固件…",
        "正在下載並驗證韌體…",
        "ファームウェアをダウンロードして検証しています…",
    ),
    (
        "Firmware updated. Your deck is ready.",
        "固件已更新，设备可以使用了。",
        "韌體已更新，裝置可以使用了。",
        "ファームウェアを更新しました。デバイスを使用できます。",
    ),
    (
        "Update did not complete. You can check again and retry.",
        "更新未完成。你可以重新检查后再试一次。",
        "更新未完成。你可以重新檢查後再試一次。",
        "更新が完了しませんでした。もう一度確認してから再試行できます。",
    ),
    (
        "Installing the temporary updater. Keep USB connected.",
        "正在安装临时更新程序，请保持 USB 连接。",
        "正在安裝臨時更新程式，請保持 USB 連線。",
        "一時的な更新プログラムをインストールしています。USB を接続したままにしてください。",
    ),
    (
        "Checking and backing up the bootloader. Keep USB connected.",
        "正在检查并备份引导程序，请保持 USB 连接。",
        "正在檢查並備份開機載入程式，請保持 USB 連線。",
        "ブートローダーを確認してバックアップしています。USB を接続したままにしてください。",
    ),
    (
        "Installing firmware. Keep USB connected.",
        "正在安装固件，请保持 USB 连接。",
        "正在安裝韌體，請保持 USB 連線。",
        "ファームウェアをインストールしています。USB を接続したままにしてください。",
    ),
    (
        "Verifying the updated device…",
        "正在验证更新后的设备…",
        "正在驗證更新後的裝置…",
        "更新したデバイスを検証しています…",
    ),
    (
        "Replacing bootloader. Do not unplug USB, power off the device, or let your computer sleep.",
        "正在替换引导程序。请勿拔下 USB、关闭设备电源或让电脑进入睡眠。",
        "正在替換開機載入程式。請勿拔除 USB、關閉裝置電源或讓電腦進入睡眠。",
        "ブートローダーを置き換えています。USB を抜いたり、デバイスの電源を切ったり、パソコンをスリープさせたりしないでください。",
    ),
    (
        "Bootloader verified. Preparing the final firmware…",
        "引导程序已验证，正在准备最终固件…",
        "開機載入程式已驗證，正在準備最終韌體…",
        "ブートローダーを検証しました。最終ファームウェアを準備しています…",
    ),
];
