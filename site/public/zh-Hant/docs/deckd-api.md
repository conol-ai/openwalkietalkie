# `deckd` v1 API 參考文件

[English](/docs/deckd-api.md) | **繁體中文**

<!-- GENERATED FILE — DO NOT EDIT. -->

本文件的英文原文依常駐程式執行時在 `GET /v1/schema` 提供的結構描述自動產生。所有方法、事件和型別的 JSON Schema 都來自 `deck-model` 的同一份 Rust 原始碼及方法分派表。每次執行 `deckd` 的 `the_checked_in_api_document_is_what_the_schema_produces` 測試時，都會比對原文與產生器的輸出。若要變更規格，請修改 Rust 型別或方法表，再重新產生文件：

```sh
cargo run --locked -p deckd --target aarch64-apple-darwin --target-dir target/host --manifest-path host/Cargo.toml -- api-doc > docs/deckd-api.md
```

[`docs/host-service.md` 第 4 節](https://github.com/conol-ai/openwalkietalkie/blob/main/docs/host-service.md#4-the-open-api) 解釋 API *為什麼*這樣設計，涵蓋傳輸方式、呼叫者身分與權限範圍、內容分類、送達規則及錯誤碼。本文件只列出結構描述本身提供的資訊，以免額外撰寫的說明過時卻無從察覺。

## 方法

`scope` 是呼叫者必須具備的最低權限範圍（§4.2）。沒有權限範圍要求的方法，在交握之前也能回覆。`answered by` 欄中，`store` 表示由狀態儲存區的 reducer 根據快照回覆，`core` 表示由核心執行緒另行回覆。`params` 連結至請求所帶參數的型別。

| 方法（method） | 權限範圍（scope） | 回覆端（answered by） | 參數（params） |
|---|---|---|---|
| `session.hello` | —（無權限範圍要求） | core | —（未列入 `Request` 列舉） |
| `session.subscribe` | —（無權限範圍要求） | core | —（未列入 `Request` 列舉） |
| `session.unsubscribe` | —（無權限範圍要求） | core | —（未列入 `Request` 列舉） |
| `session.ping` | —（無權限範圍要求） | core | —（未列入 `Request` 列舉） |
| `state.get` | `observe` | core | —（未列入 `Request` 列舉） |
| `state.events` | `observe` | core | —（未列入 `Request` 列舉） |
| `agent.list` | `observe` | core | —（未列入 `Request` 列舉） |
| `agent.get` | `observe` | core | —（未列入 `Request` 列舉） |
| `image.get` | `observe:content` | core | —（未列入 `Request` 列舉） |
| `device.get` | `observe` | core | —（未列入 `Request` 列舉） |
| `voice.get` | `observe` | core | —（未列入 `Request` 列舉） |
| `service.info` | `observe` | core | —（未列入 `Request` 列舉） |
| `sources.list` | `observe` | core | —（未列入 `Request` 列舉） |
| `agent.register` | `agent` | store | [`AgentRegister`](#agentregister) |
| `agent.heartbeat` | `agent` | store | {agents: [`AgentId`](#agentid)[]} |
| `agent.update` | `agent` | store | [`AgentUpdate`](#agentupdate) |
| `agent.companion.set` | `agent` | store | [`CompanionSet`](#companionset) |
| `agent.session.set` | `agent` | store | [`SessionSet`](#sessionset) |
| `agent.release` | `agent` | store | {agent: [`AgentId`](#agentid), reason?: string?} |
| `agent.claim` | `agent` | store | {key: string} |
| `agent.focus` | `agent` | store | {agent: [`AgentId`](#agentid)} |
| `agent.card.set` | `agent` | store | [`CardSet`](#cardset) |
| `agent.card.clear` | `agent` | store | [`CardClear`](#cardclear) |
| `agent.reply` | `agent` | store | [`ReplySet`](#replyset) |
| `agent.wait` | `agent` | core | —（未列入 `Request` 列舉） |
| `agent.order.result` | `agent` | store | [`OrderOutcome`](#orderoutcome) |
| `agent.transcribe.audio` | `agent` | core | —（未列入 `Request` 列舉） |
| `agent.transcribe.result` | `agent` | core | —（未列入 `Request` 列舉） |
| `op.answer` | `operate` | store | [`OpAnswer`](#opanswer) |
| `op.order` | `operate` | store | [`OpOrder`](#oporder) |
| `op.focus` | `operate` | store | {agent: [`AgentId`](#agentid)} |
| `op.snooze` | `operate` | store | {agent: [`AgentId`](#agentid), card: [`CardId`](#cardid)} |
| `op.ack` | `operate` | store | {agent: [`AgentId`](#agentid), card: [`CardId`](#cardid)} |
| `voice.say` | `voice` | store | [`VoiceSay`](#voicesay) |
| `voice.stop` | `voice` | store | {all?: boolean, lane?: string?, utterance?: string?} |
| `voice.listen` | `voice` | store | [`VoiceListen`](#voicelisten) |
| `voice.cancel` | `voice` | store | {} |
| `voice.engines` | `voice` | core | —（未列入 `Request` 列舉） |
| `voice.test` | `voice` | core | —（未列入 `Request` 列舉） |
| `firmware.status` | `observe` | core | —（未列入 `Request` 列舉） |
| `firmware.check` | `admin` | core | —（未列入 `Request` 列舉） |
| `firmware.install` | `admin` | core | —（未列入 `Request` 列舉） |
| `stt.status` | `voice` | core | —（未列入 `Request` 列舉） |
| `stt.install` | `admin` | core | —（未列入 `Request` 列舉） |
| `stt.remove` | `admin` | core | —（未列入 `Request` 列舉） |
| `tts.status` | `voice` | core | —（未列入 `Request` 列舉） |
| `tts.install` | `admin` | core | —（未列入 `Request` 列舉） |
| `tts.remove` | `admin` | core | —（未列入 `Request` 列舉） |
| `perm.status` | `voice` | core | —（未列入 `Request` 列舉） |
| `perm.request` | `admin` | core | —（未列入 `Request` 列舉） |
| `device.set` | `device` | store | [`DeviceSet`](#deviceset) |
| `device.faces.set` | `device` | store | {list: [`FaceEntry`](#faceentry)[]} |
| `device.usage.set` | `device` | store | {items: [`UsageItem`](#usageitem)[], period: string} |
| `device.macro.set` | `device` | store | {f: uint8, keys: [`MacroKey`](#macrokey)[], layout: string, src?: string?} |
| `device.ws.set` | `device` | store | {active: uint8, count: uint8, sel: uint8, windows: [`Window`](#window)[]} |
| `device.name` | `device` | store | {text: string} |
| `focus.report` | `device` | store | [`FocusReport`](#focusreport) |
| `config.get` | `admin` | core | —（未列入 `Request` 列舉） |
| `config.set` | `admin` | core | —（未列入 `Request` 列舉） |
| `config.reload` | `admin` | core | —（未列入 `Request` 列舉） |
| `token.create` | `admin` | core | —（未列入 `Request` 列舉） |
| `token.list` | `admin` | core | —（未列入 `Request` 列舉） |
| `token.revoke` | `admin` | core | —（未列入 `Request` 列舉） |
| `token.rotate` | `admin` | core | —（未列入 `Request` 列舉） |
| `metrics.get` | `admin` | core | —（未列入 `Request` 列舉） |
| `integrations.status` | `admin` | core | —（未列入 `Request` 列舉） |
| `integrations.install` | `admin` | core | —（未列入 `Request` 列舉） |
| `integrations.uninstall` | `admin` | core | —（未列入 `Request` 列舉） |
| `integrations.repair` | `admin` | core | —（未列入 `Request` 列舉） |
| `care.checks` | `admin` | core | —（未列入 `Request` 列舉） |
| `care.footprint` | `admin` | core | —（未列入 `Request` 列舉） |
| `care.support_file` | `admin` | core | —（未列入 `Request` 列舉） |
| `care.cache.clear` | `admin` | core | —（未列入 `Request` 列舉） |
| `care.test.question` | `admin` | core | —（未列入 `Request` 列舉） |
| `care.test.voice` | `admin` | core | —（未列入 `Request` 列舉） |
| `care.test.say` | `admin` | core | —（未列入 `Request` 列舉） |
| `care.test.status` | `admin` | core | —（未列入 `Request` 列舉） |
| `care.decks` | `admin` | core | —（未列入 `Request` 列舉） |
| `care.decks.forget` | `admin` | core | —（未列入 `Request` 列舉） |
| `log.tail` | `admin` | core | —（未列入 `Request` 列舉） |
| `device.flood` | `admin` | core | —（未列入 `Request` 列舉） |
| `device.sync` | `admin` | core | —（未列入 `Request` 列舉） |
| `service.restart` | `admin` | core | —（未列入 `Request` 列舉） |
| `service.stop` | `admin` | core | —（未列入 `Request` 列舉） |
| `grokbot.say` | `agent` | core | —（未列入 `Request` 列舉） |
| `grokbot.ask` | `agent` | core | —（未列入 `Request` 列舉） |
| `grokbot.state` | `agent` | core | —（未列入 `Request` 列舉） |
| `grokbot.status` | `admin` | core | —（未列入 `Request` 列舉） |
| `grokbot.add` | `admin` | core | —（未列入 `Request` 列舉） |
| `grokbot.remove` | `admin` | core | —（未列入 `Request` 列舉） |
| `grokbot.test` | `admin` | core | —（未列入 `Request` 列舉） |
| `context.key.set` | `admin` | core | —（未列入 `Request` 列舉） |
| `context.key.clear` | `admin` | core | —（未列入 `Request` 列舉） |
| `providers.list` | `admin` | core | —（未列入 `Request` 列舉） |
| `providers.models` | `admin` | core | —（未列入 `Request` 列舉） |
| `providers.refresh` | `admin` | core | —（未列入 `Request` 列舉） |
| `providers.test` | `admin` | core | —（未列入 `Request` 列舉） |
| `providers.key.set` | `admin` | core | —（未列入 `Request` 列舉） |
| `providers.key.clear` | `admin` | core | —（未列入 `Request` 列舉） |
| `providers.add` | `admin` | core | —（未列入 `Request` 列舉） |
| `providers.remove` | `admin` | core | —（未列入 `Request` 列舉） |
| `hooks.claude` | `hooks:ingest` | core | —（未列入 `Request` 列舉） |
| `hooks.codex` | `hooks:ingest` | core | —（未列入 `Request` 列舉） |
| `hooks.gemini` | `hooks:ingest` | core | —（未列入 `Request` 列舉） |
| `hooks.codex_notify` | `hooks:ingest` | core | —（未列入 `Request` 列舉） |
| `hooks.claude_statusline` | `hooks:ingest` | core | —（未列入 `Request` 列舉） |

## 事件

伺服器透過 JSON-RPC 通知 `{"method":"event", "params": Event}` 主動傳送事件（§4.12）。分送時會針對每個訂閱者遮蔽 `data` 中無權讀取的內容，因此各工作階段能看到什麼，取決於各自的權限範圍。

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `data` | any | 是 |  |  |
| `principal` | [`Principal`](#principal) | 是 |  |  |
| `rev` | uint64 | 是 |  |  |
| `ts` | [`Ts`](#ts) | 是 |  |  |
| `type` | [`EventKind`](#eventkind) | 是 |  |  |

`type` 的可用值如下：


## 快照

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `agents` | object | 是 |  |  |
| `device` | [`DeviceState`](#devicestate) | 是 |  |  |
| `faces` | [`FaceEntry`](#faceentry)[] | 是 |  | 最近一次採用的頁面清單（收到 `boot:1` 時還原）。 |
| `focus` | [`Focus`](#focus) | 是 |  |  |
| `macros` | object | 否 |  | 每個巨集鍵盤頁面的按鍵，依頁面 ID 區分；每次變更都會發出 `device.macros`。所有頁面都沒有設定按鍵時會省略此欄位，因此舊版用戶端也不會讀到新資料。 |
| `rev` | uint64 | 是 |  | 只有可觀察的狀態發生變化時才遞增。 |
| `service` | [`ServiceState`](#servicestate) | 是 |  |  |
| `settings` | [`Settings`](#settings) | 是 |  | 儲存在主機的設定（收到 `boot:1` 時還原）。 |
| `usage` | [`Usage`](#usage)? | 否 |  |  |
| `voice` | [`VoiceState`](#voicestate) | 是 |  |  |

## 錯誤

§4.4 的 JSON-RPC 錯誤（`code`、`message`、`data`）。

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `code` | int32 | 是 |  |  |
| `data` | any | 是 |  |  |
| `message` | string | 是 |  |  |

## 連線事件

HID 執行緒回報的連線狀況（§6.2、§6.3）。

- {link: string} — 已開啟裝置，即將傳送 `hello`。
- {link: string} — 發生讀寫錯誤、裝置已移除，或連續 6 次未收到 hello 回覆；已釋放裝置控制代碼。
- {link: string} — 連續 3 次未收到 hello 回覆，保留裝置控制代碼。
- {link: string, sid: uint32} — 收到 hello 回覆，sid 與本端相符；工作階段已就緒。
- {boot: boolean, link: string, sid: uint32} — hello 回覆的 sid 屬於另一個工作階段；正在寫入同步屏障。
- {link: string, sid: uint32} — 同步屏障的 `synced` 已寫入。
- {link: string} — 核心傳往 HID 執行緒的通道溢位；核心會根據目前狀態重新產生裝置資料。
- {link: string, pace_ms: uint16} — 兩次在密集傳輸後斷線，已延長傳送間隔（毫秒）。


## 型別

上述結構描述所參照的 92 個具名型別。

### `ActivityStatus`

可用值：`running`、`completed`、`failed`、`stopped`。


### `ActivitySummary`

僅包含供顯示的文字，不含原始工具引數或結果。

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `detail` | string | 否 | `""` |  |
| `label` | string | 是 |  |  |

### `Agent`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `caps` | [`Caps`](#caps) | 是 |  |  |
| `card` | [`Card`](#card)? | 否 |  |  |
| `chat_rev` | uint16 | 否 | `0` | 訊息持續增加時維持不變；移除較舊的保留記錄時才會變更。 |
| `companion` | [`CompanionProfile`](#companionprofile)? | 否 |  | 原生整合程式可選填的分組，以及啟動器或工作階段身分。 |
| `id` | [`AgentId`](#agentid) | 是 |  |  |
| `kind` | [`Kind`](#kind) | 是 |  |  |
| `messages` | [`ChatMessage`](#chatmessage)[] | 否 | `[]` | 近期對話，每個助理最多保留 64 則訊息、512 KiB。 |
| `meta` | any | 是 |  | 屬於內容資料。 |
| `name` | string | 是 |  |  |
| `owner` | [`Owner`](#owner) | 是 |  |  |
| `pinned` | boolean | 是 |  |  |
| `redacted` | boolean | 否 |  |  |
| `reply` | [`Reply`](#reply)? | 否 |  |  |
| `reply_rev` | uint16 | 否 | `0` | 每當 `reply` 改變就遞增。`reply` 訊息和主機排版後送出的文字區段都會帶上這個值，讓裝置分辨區段屬於目前顯示的文字還是上一版。 |
| `session` | [`SessionInfo`](#sessioninfo) | 是 |  |  |
| `since` | [`Ts`](#ts) | 是 |  | 進入 `state` 狀態的時間。 |
| `slot` | [`Slot`](#slot)? | 否 |  | 對應到裝置上的槽位；`None` 表示「不在裝置上」。 |
| `source` | [`Source`](#source) | 是 |  |  |
| `state` | [`AgentState`](#agentstate) | 是 |  |  |
| `subagents` | uint16 | 否 |  | 此工作階段正在執行的子助理（Claude 的 `SubagentStart`／`SubagentStop`，或擁有者透過 `agent.update {subagents}` 回報的數量）。助理清單顯示的數量還會加上執行中的工作流程。 |
| `task` | string? | 否 |  | 工作階段的主要任務，也是助理清單上的標題。優先使用 CLI 回報的工作階段名稱，其次是該工作階段的第一則提示詞（並非最新一則），最後才採用擁有者以 `agent.update {task}` 設定的內容。 |
| `task_source` | [`TitleSource`](#titlesource)? | 否 |  | `task` 的來源；由提示詞取得的任務屬於內容資料。 |
| `title` | string? | 否 |  | `title_source` 為 `prompt`／`tool` 時，此欄位屬於內容資料。 |
| `title_source` | [`TitleSource`](#titlesource) | 是 |  |  |
| `voice` | string? | 否 |  |  |
| `workflow` | [`Plan`](#plan)? | 否 |  |  |
| `workflow_runs` | [`Run`](#run)[] | 否 |  |  |

### `AgentId`

`"ag_…"`：在助理的整個存續期間保持不變。

型別：string。


### `AgentRegister`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `caps` | [`Caps`](#caps) | 是 |  |  |
| `cwd` | string? | 否 | `null` |  |
| `key` | string? | 否 | `null` |  |
| `kind` | [`Kind`](#kind) | 是 |  |  |
| `lease_ttl_s` | uint32 | 否 | `0` | §4.6 的 `lease_ttl_s:30` 是常駐程式的預設值，呼叫者不必主動指定。傳入 0（省略時的值）便會採用 `[agents] lease_ttl_s`。 |
| `meta` | any | 否 | `null` |  |
| `name` | string | 是 |  |  |
| `pid` | uint32? | 否 | `null` |  |
| `pid_kind` | [`PidKind`](#pidkind)? | 否 | `null` | `pid` 所指的程序（§5）：掛鉤輔助程式會向上查找父程序，CLI 程序本身標為 `cli`，以 `shell` 表示中間的 `sh -c` 程序，找不到上層 CLI 則標為 `unknown`。`kill(pid, 0)` 存活檢查**只會**輪詢 `cli`，未設定時不會輪詢。 |
| `slot_hint` | [`Slot`](#slot)? | 否 | `null` |  |
| `terminal` | [`Terminal`](#terminal)? | 否 | `null` |  |
| `voice` | string? | 否 | `null` |  |

### `AgentState`

可用值：`working`、`question`、`ready`、`error`、`idle`。


### `AgentUpdate`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | 是 |  |  |
| `meta` | any | 否 |  |  |
| `name` | string? | 否 |  |  |
| `since` | [`Ts`](#ts)? | 否 |  |  |
| `state` | [`AgentState`](#agentstate)? | 否 |  |  |
| `subagents` | uint16? | 否 |  | 工作階段目前正在執行的子助理數量。 |
| `task` | string? | 否 |  | 工作階段的主要任務（助理清單上的標題）；傳入 `""` 可清除。 |
| `title` | string? | 否 |  |  |
| `title_source` | [`TitleSource`](#titlesource)? | 否 |  |  |

### `AnswerCap`

- string
- string — 僅供轉接器使用。


### `Binding`

可用值：`left`、`right`、`enter`、`cw`、`ccw`、`shift+left`、`shift+right`、`shift+enter`。


### `Caps`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `answer` | [`AnswerCap`](#answercap) | 是 |  |  |
| `focus` | boolean | 否 | `false` | §4.15 的註冊只宣告 `caps:{answer,order}`：助理若未宣告支援取得焦點，就無法將焦點切換至該助理。 |
| `order` | [`OrderCap`](#ordercap) | 是 |  |  |
| `transcribe` | [`TranscribeCap`](#transcribecap) | 否 |  | 助理的擁有者可自行轉寫使用者對它說的話。設為 `push` 時，對該助理按住說話的錄音會以 WAV 提供給所屬連線（`agent.transcribe`），其回傳文字即為最終轉寫。常駐程式自己的辨識器會同時執行，若擁有者失敗、拒絕或逾時，就採用本機辨識結果。舊版常駐程式會忽略此欄位，整合程式可一律傳送。 |

### `Card`

§4.8 的卡片狀態機。

- {status: string}
- {choice?: uint8?, status: string, text?: string?}
- {status: string}
- {status: string}
- {reason: string, status: string}
- {status: string}
- {focused_at?: [`Ts`](#ts)?, status: string}


### `CardClear`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | 是 |  |  |
| `card` | [`CardId`](#cardid) | 是 |  |  |
| `delivered` | boolean | 是 |  |  |
| `reason` | string? | 否 | `null` |  |

### `CardId`

各槽位獨立的卡片 ID，按 `last + 1` 遞增，循環時跳過 0（§6.5）。預設的 0 表示「尚無卡片」，不會在線路上傳送。

型別：uint16。


### `CardKind`

可用值：`q`、`r`。


### `CardSet`

`agent.card.set`.

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | 是 |  |  |
| `choices` | [`Choice`](#choice)[] | 否 | `[]` |  |
| `deliver` | [`Deliver`](#deliver) | 是 |  |  |
| `free_text` | boolean | 否 | `true` |  |
| `id` | [`CardId`](#cardid)? | 否 |  | 以現有 ID 再次傳送：就地更新內容，不會再次播放提示音。 |
| `origin` | [`Origin`](#origin) | 否 | `{"group":null,"kind":"custom","request_id":null,"tool":null}` | §4.6：由權杖或指令碼建立的卡片，除非自行標明來源，否則一律為 `custom`，在裝置上會加上 `» ` 前綴。 |
| `rec` | uint8? | 否 | `null` |  |
| `text` | string | 是 |  |  |
| `timeout_s` | uint32? | 否 | `null` |  |
| `type` | [`CardKind`](#cardkind) | 是 |  |  |

### `ChatImage`

只存放小型參照。圖片本身的位元組不會出現在快照或事件環形緩衝區。

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `height` | uint32 | 是 |  |  |
| `id` | string | 是 |  |  |
| `label` | string | 是 |  |  |
| `width` | uint32 | 是 |  |  |

### `ChatMessage`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `activity` | [`ActivityStatus`](#activitystatus)? | 否 |  |  |
| `at` | [`Ts`](#ts) | 是 |  |  |
| `id` | string | 是 |  |  |
| `images` | [`ChatImage`](#chatimage)[] | 否 |  |  |
| `plan` | [`Plan`](#plan)? | 否 |  | 明確發布的公開計畫更新；一般聊天或工具訊息省略此欄位。 |
| `role` | [`ChatRole`](#chatrole) | 是 |  |  |
| `runs` | array? | 否 |  | 公開的背景工作流程計數，獨立於上層回合。 |
| `streaming` | boolean | 否 | `false` |  |
| `summary` | [`ActivitySummary`](#activitysummary)? | 否 |  | 原生整合程式提供、可公開顯示的活動標籤與預覽。 |
| `text` | string | 是 |  |  |

### `ChatRole`

可用值：`user`、`assistant`、`tool`。


### `Choice`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `detail` | string? | 否 |  |  |
| `label` | string | 是 |  |  |

### `CompanionProfile`

用戶端擁有的分組。識別碼界定哪些工作階段可調入槽位；分組標籤是公開資料。

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `app_id` | string | 是 |  | 穩定的應用程式識別碼，可含 1 至 64 個 ASCII 字母、數字、句點、連字號或底線。 |
| `group_id` | string | 是 |  | 應用程式內穩定的帳號或工作區識別碼，長度為 1 至 128 個位元組。請勿放入機密資訊。 |
| `group_label` | string | 是 |  | 助理清單中顯示的標題，長度為 1 至 24 個 UTF-8 位元組。裝置上需要分頁顯示的分組，請使用不同標題。 |
| `role` | [`CompanionRole`](#companionrole) | 是 |  |  |

### `CompanionRole`

可用值：`session`、`launcher`。


### `CompanionSet`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | 是 |  |  |
| `profile` | [`CompanionProfile`](#companionprofile)? | 否 |  | 設為 null 可移除原生顯示方式，現有註冊仍然有效。 |

### `Deliver`

- string
- string — 僅供轉接器使用：暫緩送出的掛鉤回覆。
- string — 按 ENTER 可將工作階段所屬的應用程式帶到前景（交由應用程式處理）；用於降級處理的等候狀態及資訊卡片。


### `DeviceSet`

§4.10 將這兩個欄位命名為 `k` 和 `v`，與通訊協定中的 `set` 訊息及 `device.set` 事件一致。

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `k` | [`SetKey`](#setkey) | 是 |  |  |
| `v` | uint16 | 是 |  |  |

### `DeviceState`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `battery` | uint8? | 否 |  | 裝置上次回報的電量百分比。未連線或裝置不知道電量時為 `None`；後者由裝置傳送 0 表示。 |
| `board` | string? | 否 |  | 裝置回報的主機板型號（`Rev A`、`Rev B`、`LilyGo`）。 |
| `boot` | boolean? | 否 |  |  |
| `charging` | boolean? | 否 |  | 裝置上次回報時，電池是否正接受充電電流。 |
| `deck` | string? | 否 |  | 快照中的頁面清單、設定和巨集鍵盤按鍵屬於哪一台裝置。若已連線則是目前裝置，否則是最近辨識出的那一台。以八位十六進位數字表示，每台裝置各有固定標籤，不會使用裝置位址。改為載入另一台裝置的資料時會發出 `device.profile`。辨識出第一台裝置前會省略此欄位。 |
| `drops` | [`LinkDrop`](#linkdrop)[] | 否 |  | 最近的斷線紀錄，由舊到新排列。最多 60 筆，最舊一筆與最新一筆相差不超過一小時。重新啟動後不保留。 |
| `err_counts` | [`ErrCounts`](#errcounts) | 是 |  |  |
| `face` | uint8? | 否 |  | 收到第一個 `face` 事件後才會知道。 |
| `faces` | [`FaceEntry`](#faceentry)[] | 是 |  | 裝置回報的頁面清單。 |
| `fw` | string? | 否 |  |  |
| `host_online` | boolean | 是 |  |  |
| `kinds` | uint8? | 否 |  | 已連線裝置能繪製的助理類型數（其 `hello` 中的 `kinds`）。未連線或裝置版本不支援此欄位時為 `None`。若裝置無法繪製某個助理的類型，會以 `other` 傳送（`Kind::for_deck`）。 |
| `language_supported` | boolean | 否 | `false` |  |
| `last` | [`LastLinked`](#lastlinked)? | 否 |  | 最近一次在此完成連線的裝置（有連線時就是目前裝置）；重新啟動後仍保留。 |
| `link` | [`Link`](#link) | 是 |  |  |
| `outbox_depth` | uint32 | 是 |  |  |
| `pace_ms` | uint16 | 是 |  |  |
| `session` | [`SessionState`](#sessionstate) | 是 |  |  |
| `sessions` | uint8? | 否 |  | 裝置可容納的工作階段數（其 `hello` 中的 `agents`）。裝置尚未回報時為 `None`；不支援此欄位的舊裝置可容納 8 個。 |
| `settings` | [`Settings`](#settings) | 是 |  | 最近一次從裝置收到或推送給裝置的設定。 |
| `sid` | uint32 | 是 |  | 連線使用的工作階段 ID。由此常駐程式在啟動時選定（`Store::new` 收到 ID 前為 0）；當使用者改拿另一台裝置時，會產生新的 ID。 |
| `transport` | [`Transport`](#transport)? | 否 |  | 已連線裝置使用的匯流排；未連線時為 `None`。 |

### `DormancyState`

服務的休眠狀態（`service.dormancy`）：若連續 `window_s` 秒沒有裝置連線，就停止從助理工作階段收集資料，包括輪詢工作階段、監看對話記錄與讀取用量；裝置一連線就恢復。

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `collecting` | boolean | 是 |  | 收集程序目前是否執行中。 |
| `dormant_since` | [`Ts`](#ts)? | 否 |  | 停止收集的時間，即裝置最後連線時間加上等候時段。仍在收集或此服務從未連接過裝置時為 `null`。 |
| `window_s` | uint32 | 是 |  | 裝置最後一次連線後，收集程序會再執行幾秒。 |

### `ErrCounts`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `parse` | uint32 | 是 |  |  |
| `range` | uint32 | 是 |  |  |
| `size` | uint32 | 是 |  |  |
| `unknown` | uint32 | 是 |  |  |

### `EventKind`

§4.12 的事件類型，序列化時使用以句點分隔的名稱。

- string
- string — 裝置的傳輸方式、電量、充電狀態、主機板、上次連線或斷線紀錄有變化（`device.status`）。每個事件都帶齊這六個欄位；未知值為 null，沒有斷線紀錄時為 `[]`。
- string — 裝置上的按鍵編輯器變更了一個槽位（`device.macro.edit`）。
- string — 巨集鍵盤頁面的按鍵經由 `device.macro.set` 或裝置上的編輯器變更（`device.macros`）。事件包含 `{f, src, keys}`，也就是目前 `snapshot.macros[f]` 中的完整配置。
- string — 另一台裝置已連線，快照已改為該裝置存在服務端的頁面清單、設定和巨集鍵盤按鍵（`device.profile`）。`{deck, faces, settings, macros}` 各項都是完整資料，與目前 `device.deck`、`faces`、`settings` 和 `macros` 的值相同。所有裝置共用 Mac 的語言設定，不會隨裝置切換而變更。
- string — 十分鐘沒有裝置連線，收集程序已停止，或裝置重連後已恢復收集（`service.dormancy`）。內容為 `{window_s, collecting, dormant_since}`；沒有休眠功能的服務則為 `null`。


### `FaceEntry`

頁面清單中的一個項目：頁面 ID `f`（1..=8）、範本 `t`、口述名稱 `n`。

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `f` | uint8 | 是 |  |  |
| `n` | string | 是 |  |  |
| `t` | [`Template`](#template) | 是 |  |  |

### `Focus`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid)? | 否 |  |  |
| `seq` | uint16? | 否 |  | 裝置上次傳送的 `channel.seq`。 |
| `source` | [`FocusSource`](#focussource)? | 否 |  |  |

### `FocusReport`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid)? | 否 |  |  |
| `source` | [`FocusSource`](#focussource) | 是 |  |  |

### `FocusSource`

可用值：`device`、`desktop`、`api`。


### `Group`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `count` | uint8 | 是 |  |  |
| `index` | uint8 | 是 |  |  |

### `Kind`

可用值：`claude`、`codex`、`gemini`、`other`、`conol`、`grok`。


### `LastLinked`

儲存在 `state.json` 中的裝置最後連線狀態。即使裝置不在或服務已重新啟動，仍可查到最後一次連線的時間及當時執行的版本。

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `at` | [`Ts`](#ts) | 是 |  | 仍連線時為連線就緒的時間；斷線後為連線結束的時間。 |
| `board` | string? | 否 |  |  |
| `fw` | string? | 否 |  |  |
| `transport` | [`Transport`](#transport)? | 否 |  |  |

### `Link`

可用值：`offline`、`connected`、`degraded`。


### `LinkDrop`

一筆斷線紀錄：發生時間與 HID 執行緒回報的原因（`read`、`write`、`write_stall`、`removed`、`misses`）。

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `at` | [`Ts`](#ts) | 是 |  |  |
| `why` | string | 是 |  |  |

### `Listener`

常駐程式無法取得 TCP 連接埠時，記錄占用它的程序（依 §4.1 以 `lsof -nP -iTCP:<port> -sTCP:LISTEN` 查詢，盡力取得結果）。

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `command` | string? | 否 |  |  |
| `pid` | uint64 | 是 |  |  |

### `MacroKey`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `binding` | [`Binding`](#binding)? | 否 |  | 觸發這個鍵的裝置實體控制項；`None` 代表只能透過觸控，以及已設定的按鍵組合（如有）觸發。 |
| `keys` | string? | 否 |  | 按下此鍵時，常駐程式送出的按鍵組合，例如 `cmd+shift+4`（`keystroke_valid`）。`None` 表示只送出事件。 |
| `label` | string | 是 |  |  |

### `MacroLayout`

主機保存的巨集鍵盤頁面配置，可由 `device.macro.set` 或裝置本身的編輯器設定，並寫入 `state.json` 供重新啟動後還原。裝置一次只回報一項變更，不會送出整組按鍵，所以這是唯一的完整副本。

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `keys` | [`MacroKey`](#macrokey)[] | 是 |  | 從槽位 0 開始，最多九個。空白標籤代表空槽位，後續按鍵的位置不會因此改變。 |
| `src` | string? | 否 |  | 裝置在巨集鍵盤標頭顯示的標籤，以最近一次提供的值為準；未提供時為 `None`。 |

### `MessageMeta`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `activity` | [`ActivityStatus`](#activitystatus)? | 否 |  |  |
| `id` | string | 是 |  |  |
| `images` | [`ChatImage`](#chatimage)[] | 否 |  |  |
| `plan` | [`Plan`](#plan)? | 否 |  | 明確發布的公開計畫更新；一般聊天或工具訊息省略此欄位。 |
| `role` | [`ChatRole`](#chatrole) | 是 |  |  |
| `runs` | array? | 否 |  | 公開的背景工作流程計數，獨立於上層回合。 |
| `streaming` | boolean | 否 | `false` |  |
| `summary` | [`ActivitySummary`](#activitysummary)? | 否 |  | 原生整合程式提供、可公開顯示的活動標籤與預覽。 |

### `ModelOption`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `default_effort` | string | 否 | `""` |  |
| `efforts` | string[] | 否 | `[]` |  |
| `id` | string | 是 |  |  |
| `label` | string | 是 |  |  |

### `OpAnswer`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | 是 |  |  |
| `card` | [`CardId`](#cardid) | 是 |  |  |
| `choice` | uint8? | 否 | `null` |  |
| `text` | string? | 否 | `null` |  |

### `OpOrder`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | 是 |  |  |
| `ctx` | string | 是 |  |  |
| `source` | [`OrderSource`](#ordersource) | 是 |  |  |
| `text` | string | 是 |  |  |
| `utterance` | string? | 否 | `null` | 口述這道指令時的 PTT 語句（B12）。§4.12 為 `agent.order` 定義了 `utterance?`，[`crate::Order`] 從第一週便有這個欄位，但先前兩處都無法填值：唯一會產生命令的請求沒有位置存放 ID。用戶端若想將指令對應到其來源 `transcript.final`，只能比對文字，而 §5.6 的文法規則恰好可能改寫文字。鍵盤輸入（`source:"gui"`、`"api"`）省略此欄位；使用 `default`，既有呼叫端不必傳送。 |

### `OrderCap`

- string
- string — 僅供轉接器使用。


### `OrderOutcome`

常駐程式將指令交給助理擁有者後的送達結果。`op.order` 送出的每一道指令，不論口述或打字，都會連同 `order` ID 交給擁有者：`agent.order` 通知與 `agent.wait` 的 `{kind:"order"}` 結果都含有 ID。在「已完成」卡片上選擇步驟（`ctx:"step"`）則沒有 ID，由裝置自行還原。擁有者若無法將指令送達對話，須以 `ok: false` 和 `reason` 回報一次，裝置會顯示未送達。`ok: true` 會被接受，但不會改變狀態。只有助理所屬的工作階段可以回報（其他工作階段會收到 `-32004`），且限十分鐘內尚未回報的指令（否則為 `-32003`）。`reason` 也須符合下述單字格式（否則為 `-32009`）。結果為 `{rev, shown}`；依提供給裝置的呈現方式，可能是 `card`、`toast` 或 `none`。

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | 是 |  | 這道指令的目標助理。 |
| `ok` | boolean | 是 |  | 指令是否已送達對話。 |
| `order` | string | 是 |  | 指令所帶的 `order` ID。 |
| `reason` | string? | 否 | `null` | 未送達的原因：一個由小寫字母、數字及底線組成的單字，最多 32 個位元組（例如 `backend_refused`）。省略時使用 `undeliverable`。 |

### `OrderSource`

可用值：`ptt`、`gui`、`api`。


### `Origin`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `group` | [`Group`](#group)? | 否 |  |  |
| `kind` | [`OriginKind`](#originkind) | 是 |  |  |
| `request_id` | string? | 否 |  |  |
| `tool` | string? | 否 |  |  |

### `OriginKind`

- string
- string — 僅供內建轉接器使用。
- string — 任何權杖或指令碼均可使用；裝置上會加上 `» ` 前綴。


### `Owner`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `lease_expires` | [`Ts`](#ts)? | 否 |  |  |
| `principal` | [`Principal`](#principal) | 是 |  |  |
| `session` | [`SessionId`](#sessionid) | 是 |  |  |

### `Page`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `count` | uint8 | 是 |  |  |
| `index` | uint8 | 是 |  |  |

### `Permission`

可用值：`unknown`、`not_determined`、`granted`、`denied`。


### `Permissions`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `bluetooth` | [`Permission`](#permission) | 否 | `"unknown"` | BLE 語音連線最近讀取到的此程序 CoreBluetooth 授權狀態（`[voice] transport = "ble"`）。使用這個傳輸方式時，裝置麥克風與揚聲器需要藍牙授權，而非麥克風授權。使用 CoreAudio 傳輸或連線尚未啟動時為 `unknown`。 |
| `microphone` | [`Permission`](#permission) | 是 |  |  |
| `speech` | [`Permission`](#permission) | 是 |  |  |

### `PidKind`

可用值：`cli`、`shell`、`unknown`。


### `Plan`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `explanation` | string? | 否 |  |  |
| `steps` | [`PlanStep`](#planstep)[] | 是 |  |  |

### `PlanStep`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `status` | [`StepStatus`](#stepstatus) | 是 |  |  |
| `step` | string | 是 |  |  |

### `Principal`

型別：string。


### `PttCtx`

可用值：`talk`、`answer`、`name`。


### `PttInfo`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid)? | 否 |  |  |
| `card` | [`CardId`](#cardid)? | 否 |  |  |
| `companion` | boolean | 否 |  | 這次按住說話的內容由助理擁有者轉寫（`caps.transcribe`）。本機辨識器僅在必要時接手，因此裝置不會顯示它的部分辨識結果。 |
| `ctx` | [`PttCtx`](#pttctx) | 是 |  |  |
| `live` | boolean | 否 | `false` | 收音是否已產生**非零取樣值**。這用來區分 §7.4 的 `Arming`（false）與 `Listening`（true）。兩者相差約一秒，使用者確實會感受到：此硬體啟動 SCO 的時間中位數為 1031 毫秒（V1，2026-09-21 量測）。這段期間已有回呼，`AudioUnitRender` 也會傳回 `noErr`，但每個數位取樣值都恰好為零。此時顯示「正在聆聽」會誤導使用者；若只憑回呼判斷麥克風正常，收不到聲音時也永遠不會觸發異常檢查（V2）。 |
| `principal` | [`Principal`](#principal) | 是 |  | 啟動此工作階段的一方。`device` 代表由裝置上的 `ptt` 按鍵事件啟動，其他情況為 `voice.listen` 的呼叫者（§7.4）。 |
| `session` | [`SessionId`](#sessionid)? | 否 | `null` | 呼叫者當時所在的**工作階段**，單憑 `principal` 無法得知（B2）。§4.9 要求分派器產生的每個 `op.answer`／`op.order` 同時包含身分及工作階段。此常駐程式按工作階段判斷擁有權，不按身分判斷（§4.2）；沒有此欄位，就無法把口述回答歸屬到提出請求的連線，也無法將結果交給所屬工作階段上的 `agent.wait`。由裝置按鍵觸發時並沒有呼叫者工作階段，因此為 `None`。 |
| `since` | [`Ts`](#ts) | 是 |  |  |
| `utterance` | string | 否 | `""` | 此次工作階段的所有轉寫都會帶上的 ID（`ut_<n>`、§4.12 的 `transcript.partial\|final{utterance}` 與 B12 的 `order.dispatched`）。開始收音時即配置，因為部分轉寫會在其他元件來得及命名前開始送達。 |

### `Reply`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `announce` | string? | 否 |  |  |
| `at` | [`Ts`](#ts) | 是 |  |  |
| `redacted` | boolean | 否 |  |  |
| `text` | string? | 否 |  | 屬於內容資料。 |

### `ReplySet`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | 是 |  |  |
| `announce` | string? | 否 | `null` |  |
| `lane` | string? | 否 | `null` |  |
| `message` | [`MessageMeta`](#messagemeta)? | 否 | `null` | 服務商提供穩定的項目 ID，用來在串流期間更新同一則訊息。 |
| `speak` | boolean | 否 | `false` |  |
| `text` | string | 是 |  |  |

### `Route`

§7.2 的輸出路徑狀態機。B7：過去使用 `Sco` 表示 §7.2 的 **`ScoOurs`**，而且沒有 `Reopening` 狀態，因此每次放開按鍵後約兩秒半所處的狀態沒有名稱（V1：A2DP 恢復時間的**中位數為 2307 毫秒**），`voice.route` 也無法回報。`ScoOurs` 的區分十分重要：若是*本程式*開啟麥克風而進入 SCO，放開按鍵便可退出；若是其他應用程式開啟的，則只能等待（V7：此處裝置是系統預設輸入，這是常見情況）。

- string
- string — 輸出以 44.1 kHz 開啟並持續保持開啟（§7.2）。
- string — 本程式正在以 PTT 收音；所有播放通道暫停，輸出關閉。
- string — 其他應用程式收音，導致目前取樣率改變。
- string — 本程式結束收音後、恢復 A2DP 前的過渡狀態。V3：立即以裝置**目前的**標稱取樣率開啟，首次算繪回呼只需 48 毫秒；若等到 44100 則需 2591 毫秒。每次放開 PTT 後的播報相差 54 倍；這 2.3 秒是 macOS 拆除 SCO 連線的固定時間，無法縮短。
- string — 重新開啟已失敗兩次（§7.2）。輸出以錯誤的取樣率開啟並重新取樣，此時 `voice.route{stuck:true}`；下一次取樣率變更時恢復。
- string — `[voice] fallback_output = "system_default"`：使用系統預設輸出，不經過裝置。
- string — BLE 語音連線（`[voice] transport = "ble"`）：裝置的揚聲器特徵值以 16 kHz 開啟，並保持開啟。這是該傳輸方式的穩定狀態，對應傳統藍牙裝置的 `A2dp`。
- string — 本程式透過 BLE 語音連線以 PTT 收音。按住期間會關閉裝置揚聲器（半雙工，沒有回音消除，V5），放開後重新開啟。這是 BLE 傳輸中對應 `ScoOurs` 的狀態。


### `Run`

服務商回報的背景執行工作。計數代表工作程序數，而非推測的計畫步驟數。

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `completed` | uint32 | 是 |  |  |
| `elapsed_ms` | uint64 | 是 |  |  |
| `id` | string | 是 |  |  |
| `name` | string | 是 |  |  |
| `status` | [`ActivityStatus`](#activitystatus) | 是 |  |  |
| `tokens` | uint64 | 是 |  |  |
| `total` | uint32 | 是 |  |  |

### `ServiceState`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `degraded` | string? | 否 |  |  |
| `dormancy` | [`DormancyState`](#dormancystate)? | 否 | `null` | 收集程序的休眠狀態。不支援休眠的服務為 `null`，包括以 `--no-device` 啟動的服務與加入此功能前的舊版。 |
| `listener` | [`Listener`](#listener)? | 否 | `null` | 當 `degraded` 是 `port_in_use`／`hooks_hijack_risk` 時設定：該連接埠所屬的程序，供 `service.degraded` 與 `service.info` 使用（§4.1、§4.12）。 |
| `port` | uint16 | 是 |  |  |
| `version` | string | 是 |  |  |

### `SessionControls`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `editable` | boolean | 否 | `false` |  |
| `effort` | string | 是 |  |  |
| `model` | string | 是 |  |  |
| `models` | [`ModelOption`](#modeloption)[] | 否 | `[]` |  |
| `note` | string | 否 | `""` |  |

### `SessionId`

型別：string。


### `SessionInfo`

執行助理的位置（除 `key` 外均屬於內容資料）。

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `cwd` | string? | 否 |  |  |
| `key` | string? | 否 |  | `"claude:<session_id>"`、`"myapp:<id>"`：避免重複執行的識別碼（idempotency key）。 |
| `pid` | uint32? | 否 |  |  |
| `pid_kind` | [`PidKind`](#pidkind)? | 否 |  |  |
| `terminal` | [`Terminal`](#terminal)? | 否 |  |  |

### `SessionSet`

已驗證擁有者身分的設定回報。一般重新整理時省略 `request_rev`。

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | 是 |  |  |
| `controls` | [`SessionControls`](#sessioncontrols) | 是 |  |  |
| `request_rev` | uint16? | 否 | `null` | 套用變更後，須對應 `session.settings_requested` 的確切修訂版本確認。若變更失敗，請回報實際設定，並附上非空白的說明。 |

### `SessionState`

§6.3.

可用值：`absent`、`linked`、`await_hello`、`syncing`、`live`、`degraded`。


### `SetKey`

- string
- string — API 設定；透過附版本資訊的 `language` 訊息傳送給裝置。
- string — 裝置顯示所收到文件的字級：0 小、1 中、2 大。**主機**需要知道這個值，因為文字由主機負責換行；兩端採用不同字級，每行的斷行位置就會不同。


### `Settings`

主機持久儲存的 `set` 設定鍵，收到 `boot:1` 時會還原。

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `announce` | uint16 | 否 | `1` |  |
| `arcadesound` | uint16 | 否 | `0` | 0 關閉、1 僅音效、2 音樂與音效；可在「遊戲」選單中變更。 |
| `autojump` | uint16 | 否 | `0` |  |
| `brightness` | uint16 | 否 | `5` |  |
| `chime` | uint16 | 否 | `1` |  |
| `demo` | uint16 | 否 | `0` |  |
| `idlehome` | uint16 | 否 | `120` |  |
| `language` | uint16 | 否 | `0` | 與支援此功能的裝置同步的介面語言：en、zh-Hans、zh-Hant、ja。 |
| `language_revision` | uint32 | 否 | `0` |  |
| `mic` | uint16 | 否 | `6` |  |
| `speaker` | uint16 | 否 | `6` |  |
| `textsize` | uint16 | 否 | `1` | 0 小、1 中、2 大。**主機**會依此處選用的字型為文件換行，因此這不只是裝置本身的偏好設定。 |

### `Slot`

裝置槽位 1..=255（線路上佔一個位元組）。裝置共有 `DeviceState::capacity` 個槽位；未回報數量的裝置有 8 個。

型別：uint8。


### `Source`

可用值：`hook`、`api`、`mcp`。


### `Speaking`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid)? | 否 |  |  |
| `lane` | string | 是 |  |  |
| `utterance` | string | 是 |  |  |

### `StepStatus`

可用值：`pending`、`in_progress`、`completed`。


### `SttInfo`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `engine` | string | 是 |  |  |
| `locale` | string | 是 |  |  |
| `on_device` | boolean | 是 |  |  |

### `Template`

可用值：`agents`、`omarchy`、`macropad`、`usage`、`settings`、`arcade`、`system`、`timer`。


### `Terminal`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `app` | string? | 否 |  | 工作階段所屬應用程式的 macOS 套件識別碼（`dev.zed.Zed`、`com.mitchellh.ghostty`、`com.anthropic.claudefordesktop`），由掛鉤輔助程式查得。在工作階段的 `Open app` 卡片上按鍵，即可將該應用程式帶到前景。若助理由已連線的應用程式註冊，此欄位僅供參考；常駐程式只會將透過掛鉤註冊的工作階段所屬應用程式帶到前景。 |
| `inbox` | string? | 否 |  | 工作階段專用的訊息收件匣，也就是 Claude Code 為跨工作階段訊息繫結的 Unix 通訊端（`CLAUDE_CODE_MESSAGING_SOCKET`），取自掛鉤輔助程式的環境變數。助理有這個收件匣時，指令會直接送入通訊端，無須輸入到終端機（§5.5），因此不必切換焦點、執行 AppleScript 或取得 TCC 授權。隨通訊端提供的工作階段權杖**不會**出現在這裡：它只保存在常駐程式內部（`adapters::rt::Adapters`），不會寫入快照。 |
| `iterm_session` | string? | 否 |  |  |
| `program` | string? | 否 |  |  |
| `term_session` | string? | 否 |  |  |
| `title_stamp` | string? | 否 |  | 移除 Ghostty 分頁路徑後便不再寫入此欄位，保留是為了讓仍傳送它的用戶端能正確解析。 |
| `tmux_pane` | string? | 否 |  |  |
| `tty` | string? | 否 |  |  |

### `TitleSource`

- string — `session_name`／`name`：屬於狀態資料。
- string — 來自目前工作目錄（cwd）：屬於狀態資料。
- string — 使用者提示詞開頭的 64 B：屬於內容資料。
- string — 工具輸入的開頭：屬於內容資料。
- string — 透過 `agent.update` 設定：屬於狀態資料。


### `Tone`

可用值：`blue`、`amber`、`green`、`red`、`gray`。


### `TranscribeCap`

誰有權將按住說話的錄音轉寫成文字（`caps.transcribe`）。

- string — 每次使用者對這個助理口述時，其擁有者都會收到 `agent.transcribe`，並以 `agent.transcribe.result` 回覆。
- string — 僅使用常駐程式自己的辨識器。


### `Transport`

裝置如何連接這台電腦：HID 執行緒開啟該裝置時使用的匯流排。

可用值：`usb`、`bluetooth`。


### `Ts`

型別：string。


### `TtsInfo`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `engine` | string | 是 |  |  |

### `Usage`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `at` | [`Ts`](#ts)? | 否 |  |  |
| `items` | [`UsageItem`](#usageitem)[] | 是 |  |  |
| `period` | string | 是 |  |  |

### `UsageItem`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `account` | string? | 否 |  | 這些用量時段所屬的帳號；僅在資料來源提供時才有值。 |
| `name` | string | 是 |  |  |
| `percent` | uint8 | 是 |  | 0..=100. |
| `reset` | string | 是 |  |  |
| `reset_tip` | string? | 否 |  | 即將到期且可兌換的重設額度提醒；沒有即將到期的額度時省略。 |
| `tone` | [`Tone`](#tone)? | 否 |  | `None` 表示依 `percent` 決定用量條的顏色。 |

### `VoiceInput`

PTT 工作階段要開啟的麥克風（§4.9 `voice.listen{input}`）。

- string — 裝置本身的輸入 UID；裝置的 `ptt` 按鍵事件只會指定這個輸入。
- string — Mac 的預設輸入裝置。只有 `voice.listen{input:"mac"}` 會指定這個輸入；按下裝置按鍵時不會改用它。


### `VoiceListen`

`voice.listen`（§4.9）是圖形介面與 API 進入 §7.4 狀態機的入口；裝置的 `ptt` 按鍵事件也會驅動同一套狀態機。所有衍生事件皆使用**呼叫者的**身分與工作階段，不會使用 `device`（B2）。

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | 是 |  |  |
| `card` | [`CardId`](#cardid)? | 否 | `null` |  |
| `ctx` | string | 否 | `"talk"` | `"talk"` 或 `"answer"`。使用字串而非 [`crate::PttCtx`]，因為 §4.9 只允許這兩個值：`name` 屬於裝置按鍵事件，API 呼叫者不得自行產生。 |
| `input` | [`VoiceInput`](#voiceinput) | 否 | `"deck"` | `"deck"`（預設）或 `"mac"`，亦即 §4.9 的 `input`。 |
| `max_s` | uint32 | 否 | `60` |  |

### `VoiceSay`

`voice.say` (§4.9).

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid)? | 否 | `null` |  |
| `interrupt` | boolean | 否 | `false` | §4.9：不可搶占優先順序較高的通道，由混音器決定。 |
| `lane` | string | 是 |  |  |
| `text` | string | 是 |  |  |
| `ttl_s` | uint32? | 否 | `null` | §4.9 的 `ttl_s?:30`：若 `chatter` 在期限前尚未開始播放，就會捨棄。期限會一直傳到混音器，不會在這裡被忽略；呼叫者要求設定期限卻被悄悄忽略，比直接拒絕更難處理。 |
| `voice` | string? | 否 | `null` |  |

### `VoiceState`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `input_uid` | string? | 否 |  |  |
| `output_uid` | string? | 否 |  |  |
| `permissions` | [`Permissions`](#permissions) | 是 |  |  |
| `ptt` | [`PttInfo`](#pttinfo)? | 否 |  |  |
| `route` | [`Route`](#route) | 是 |  |  |
| `speaking` | [`Speaking`](#speaking)? | 否 |  |  |
| `stt` | [`SttInfo`](#sttinfo) | 是 |  |  |
| `transport` | [`VoiceTransport`](#voicetransport)? | 否 | `null` | 音訊引擎建置時選定的傳輸方式（`[voice] transport`）。引擎尚未回報時，或此組建不含引擎時，值為 null。 |
| `tts` | [`TtsInfo`](#ttsinfo) | 是 |  |  |

### `VoiceTransport`

裝置音訊使用的傳輸路徑（`[voice] transport`）：透過 BLE 連線使用裝置本身的 GATT 音訊服務（`deckd::ble`），或透過 CoreAudio 存取 Rev A 之前的傳統藍牙 HFP/A2DP 裝置對。

- string
- string — Rev A 韌體已不再是傳統藍牙音訊裝置，因此預設使用此傳輸方式。


### `Window`

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|---|---|---|---|---|
| `title` | string | 是 |  |  |
| `tone` | [`Tone`](#tone)? | 否 |  | 省略時顯示為灰色。 |
