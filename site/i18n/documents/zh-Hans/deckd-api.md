# `deckd` — v1 API

[English](/docs/deckd-api.md) | **简体中文**

<!-- Translation of the generated English API reference. Update via i18n/documents/. -->

本文件的英文原文由运行中的守护进程在 `GET /v1/schema` 提供的内容生成：包括每个方法、事件和类型的 JSON Schema，均来自 `deck-model` 中同一份 Rust 源码，以及方法分发表。直接编辑原文没有意义：每次运行 `deckd` 测试 `the_checked_in_api_document_is_what_the_schema_produces` 时，都会比较文件与生成器的输出。需要修改时，应先更改 Rust 类型或方法表，再重新生成：

```sh
cargo run --locked -p deckd --target aarch64-apple-darwin --target-dir target/host --manifest-path host/Cargo.toml -- api-doc > docs/deckd-api.md
```

有关 API 设计原因的说明，包括传输方式、访问主体和权限范围、内容分类、交付约定及错误码，见 [`docs/host-service.md` 第 4 节](https://github.com/conol-ai/openwalkietalkie/blob/main/docs/host-service.md#4-the-open-api)。本文件只记录 schema 中已有的信息，避免额外说明过时却无法被检查发现。

## 方法

`scope` 是访问主体必须具备的最低权限范围（第 4.2 节）；未要求权限范围的方法可在握手前调用。`answered by` 为 `store` 时，由状态存储的 reducer 根据快照应答；为 `core` 时，由核心线程在该处理流程之外应答。`params` 链接到请求携带的参数类型。

| method | scope | 应答方 | params |
|---|---|---|---|
| `session.hello` | —（无需权限范围） | core | —（不在 `Request` 枚举中） |
| `session.subscribe` | —（无需权限范围） | core | —（不在 `Request` 枚举中） |
| `session.unsubscribe` | —（无需权限范围） | core | —（不在 `Request` 枚举中） |
| `session.ping` | —（无需权限范围） | core | —（不在 `Request` 枚举中） |
| `state.get` | `observe` | core | —（不在 `Request` 枚举中） |
| `state.events` | `observe` | core | —（不在 `Request` 枚举中） |
| `agent.list` | `observe` | core | —（不在 `Request` 枚举中） |
| `agent.get` | `observe` | core | —（不在 `Request` 枚举中） |
| `image.get` | `observe:content` | core | —（不在 `Request` 枚举中） |
| `device.get` | `observe` | core | —（不在 `Request` 枚举中） |
| `voice.get` | `observe` | core | —（不在 `Request` 枚举中） |
| `service.info` | `observe` | core | —（不在 `Request` 枚举中） |
| `sources.list` | `observe` | core | —（不在 `Request` 枚举中） |
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
| `agent.wait` | `agent` | core | —（不在 `Request` 枚举中） |
| `agent.order.result` | `agent` | store | [`OrderOutcome`](#orderoutcome) |
| `agent.transcribe.audio` | `agent` | core | —（不在 `Request` 枚举中） |
| `agent.transcribe.result` | `agent` | core | —（不在 `Request` 枚举中） |
| `op.answer` | `operate` | store | [`OpAnswer`](#opanswer) |
| `op.order` | `operate` | store | [`OpOrder`](#oporder) |
| `op.focus` | `operate` | store | {agent: [`AgentId`](#agentid)} |
| `op.snooze` | `operate` | store | {agent: [`AgentId`](#agentid), card: [`CardId`](#cardid)} |
| `op.ack` | `operate` | store | {agent: [`AgentId`](#agentid), card: [`CardId`](#cardid)} |
| `voice.say` | `voice` | store | [`VoiceSay`](#voicesay) |
| `voice.stop` | `voice` | store | {all?: boolean, lane?: string?, utterance?: string?} |
| `voice.listen` | `voice` | store | [`VoiceListen`](#voicelisten) |
| `voice.cancel` | `voice` | store | {} |
| `voice.engines` | `voice` | core | —（不在 `Request` 枚举中） |
| `voice.test` | `voice` | core | —（不在 `Request` 枚举中） |
| `firmware.status` | `observe` | core | —（不在 `Request` 枚举中） |
| `firmware.check` | `admin` | core | —（不在 `Request` 枚举中） |
| `firmware.install` | `admin` | core | —（不在 `Request` 枚举中） |
| `stt.status` | `voice` | core | —（不在 `Request` 枚举中） |
| `stt.install` | `admin` | core | —（不在 `Request` 枚举中） |
| `stt.remove` | `admin` | core | —（不在 `Request` 枚举中） |
| `tts.status` | `voice` | core | —（不在 `Request` 枚举中） |
| `tts.install` | `admin` | core | —（不在 `Request` 枚举中） |
| `tts.remove` | `admin` | core | —（不在 `Request` 枚举中） |
| `perm.status` | `voice` | core | —（不在 `Request` 枚举中） |
| `perm.request` | `admin` | core | —（不在 `Request` 枚举中） |
| `device.set` | `device` | store | [`DeviceSet`](#deviceset) |
| `device.faces.set` | `device` | store | {list: [`FaceEntry`](#faceentry)[]} |
| `device.usage.set` | `device` | store | {items: [`UsageItem`](#usageitem)[], period: string} |
| `device.macro.set` | `device` | store | {f: uint8, keys: [`MacroKey`](#macrokey)[], layout: string, src?: string?} |
| `device.ws.set` | `device` | store | {active: uint8, count: uint8, sel: uint8, windows: [`Window`](#window)[]} |
| `device.name` | `device` | store | {text: string} |
| `focus.report` | `device` | store | [`FocusReport`](#focusreport) |
| `config.get` | `admin` | core | —（不在 `Request` 枚举中） |
| `config.set` | `admin` | core | —（不在 `Request` 枚举中） |
| `config.reload` | `admin` | core | —（不在 `Request` 枚举中） |
| `token.create` | `admin` | core | —（不在 `Request` 枚举中） |
| `token.list` | `admin` | core | —（不在 `Request` 枚举中） |
| `token.revoke` | `admin` | core | —（不在 `Request` 枚举中） |
| `token.rotate` | `admin` | core | —（不在 `Request` 枚举中） |
| `metrics.get` | `admin` | core | —（不在 `Request` 枚举中） |
| `integrations.status` | `admin` | core | —（不在 `Request` 枚举中） |
| `integrations.install` | `admin` | core | —（不在 `Request` 枚举中） |
| `integrations.uninstall` | `admin` | core | —（不在 `Request` 枚举中） |
| `integrations.repair` | `admin` | core | —（不在 `Request` 枚举中） |
| `care.checks` | `admin` | core | —（不在 `Request` 枚举中） |
| `care.footprint` | `admin` | core | —（不在 `Request` 枚举中） |
| `care.support_file` | `admin` | core | —（不在 `Request` 枚举中） |
| `care.cache.clear` | `admin` | core | —（不在 `Request` 枚举中） |
| `care.test.question` | `admin` | core | —（不在 `Request` 枚举中） |
| `care.test.voice` | `admin` | core | —（不在 `Request` 枚举中） |
| `care.test.say` | `admin` | core | —（不在 `Request` 枚举中） |
| `care.test.status` | `admin` | core | —（不在 `Request` 枚举中） |
| `care.decks` | `admin` | core | —（不在 `Request` 枚举中） |
| `care.decks.forget` | `admin` | core | —（不在 `Request` 枚举中） |
| `log.tail` | `admin` | core | —（不在 `Request` 枚举中） |
| `device.flood` | `admin` | core | —（不在 `Request` 枚举中） |
| `device.sync` | `admin` | core | —（不在 `Request` 枚举中） |
| `service.restart` | `admin` | core | —（不在 `Request` 枚举中） |
| `service.stop` | `admin` | core | —（不在 `Request` 枚举中） |
| `grokbot.say` | `agent` | core | —（不在 `Request` 枚举中） |
| `grokbot.ask` | `agent` | core | —（不在 `Request` 枚举中） |
| `grokbot.state` | `agent` | core | —（不在 `Request` 枚举中） |
| `grokbot.status` | `admin` | core | —（不在 `Request` 枚举中） |
| `grokbot.add` | `admin` | core | —（不在 `Request` 枚举中） |
| `grokbot.remove` | `admin` | core | —（不在 `Request` 枚举中） |
| `grokbot.test` | `admin` | core | —（不在 `Request` 枚举中） |
| `context.key.set` | `admin` | core | —（不在 `Request` 枚举中） |
| `context.key.clear` | `admin` | core | —（不在 `Request` 枚举中） |
| `providers.list` | `admin` | core | —（不在 `Request` 枚举中） |
| `providers.models` | `admin` | core | —（不在 `Request` 枚举中） |
| `providers.refresh` | `admin` | core | —（不在 `Request` 枚举中） |
| `providers.test` | `admin` | core | —（不在 `Request` 枚举中） |
| `providers.key.set` | `admin` | core | —（不在 `Request` 枚举中） |
| `providers.key.clear` | `admin` | core | —（不在 `Request` 枚举中） |
| `providers.add` | `admin` | core | —（不在 `Request` 枚举中） |
| `providers.remove` | `admin` | core | —（不在 `Request` 枚举中） |
| `hooks.claude` | `hooks:ingest` | core | —（不在 `Request` 枚举中） |
| `hooks.codex` | `hooks:ingest` | core | —（不在 `Request` 枚举中） |
| `hooks.gemini` | `hooks:ingest` | core | —（不在 `Request` 枚举中） |
| `hooks.codex_notify` | `hooks:ingest` | core | —（不在 `Request` 枚举中） |
| `hooks.claude_statusline` | `hooks:ingest` | core | —（不在 `Request` 枚举中） |

## 事件

服务器以 JSON-RPC 通知 `{"method":"event", "params": Event}` 推送事件（第 4.12 节）。分发时，会根据每个订阅者的权限对 `data` 进行脱敏，因此会话能看到哪些内容取决于其权限范围。

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `data` | any | 是 |  |  |
| `principal` | [`Principal`](#principal) | 是 |  |  |
| `rev` | uint64 | 是 |  |  |
| `ts` | [`Ts`](#ts) | 是 |  |  |
| `type` | [`EventKind`](#eventkind) | 是 |  |  |

`type` 的取值包括：


## 快照

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `agents` | object | 是 |  |  |
| `device` | [`DeviceState`](#devicestate) | 是 |  |  |
| `faces` | [`FaceEntry`](#faceentry)[] | 是 |  | 最近一次采用的页面列表（在 `boot:1` 时恢复）。 |
| `focus` | [`Focus`](#focus) | 是 |  |  |
| `macros` | object | 否 |  | 按界面 ID 保存各个宏键盘页面的按键；每次变更都会由 `device.macros` 通知。所有界面均未设置按键时，此字段缺省，旧版读取方就不会遇到新字段。 |
| `rev` | uint64 | 是 |  | 仅在可观察状态发生变化时递增。 |
| `service` | [`ServiceState`](#servicestate) | 是 |  |  |
| `settings` | [`Settings`](#settings) | 是 |  | 主机持久保存的设置（在 `boot:1` 时恢复）。 |
| `usage` | [`Usage`](#usage)? | 否 |  |  |
| `voice` | [`VoiceState`](#voicestate) | 是 |  |  |

## 错误

第 4.4 节定义的 JSON-RPC 错误（`code`、`message`、`data`）。

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `code` | int32 | 是 |  |  |
| `data` | any | 是 |  |  |
| `message` | string | 是 |  |  |

## 连接事件

来自 HID 线程（第 6.2、6.3 节），描述连接发生的变化。

- {link: string} — 已打开设备，正在发送 `hello`。
- {link: string} — 读写出错、设备移除或连续 6 次未收到 hello：丢弃句柄。
- {link: string} — 连续 3 次未收到 hello，保留句柄。
- {link: string, sid: uint32} — 收到带有本方 sid 的 hello 回复：会话已生效。
- {boot: boolean, link: string, sid: uint32} — 收到带有其他 sid 的 hello 回复：正在写入同步屏障。
- {link: string, sid: uint32} — 已写入屏障的 `synced`。
- {link: string} — core→hid 通道溢出，核心重新投影状态。
- {link: string, pace_ms: uint16} — 突发传输后两次掉线，增大传输间隔（毫秒）。


## 类型

上述 schema 引用的 92 个命名类型。

### `ActivityStatus`

可选值：`running`、`completed`、`failed`、`stopped`。


### `ActivitySummary`

仅为显示用文本，不是原始工具参数或结果。

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `detail` | string | 否 | `""` |  |
| `label` | string | 是 |  |  |

### `Agent`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `caps` | [`Caps`](#caps) | 是 |  |  |
| `card` | [`Card`](#card)? | 否 |  |  |
| `chat_rev` | uint16 | 否 | `0` | 追加消息不会改变此值；清理较早的历史消息时，它才会改变。 |
| `companion` | [`CompanionProfile`](#companionprofile)? | 否 |  | 可选的原生应用分组，以及启动器或会话标识。 |
| `id` | [`AgentId`](#agentid) | 是 |  |  |
| `kind` | [`Kind`](#kind) | 是 |  |  |
| `messages` | [`ChatMessage`](#chatmessage)[] | 否 | `[]` | 最近的对话，每个助手最多保留 64 条消息、512 KiB。 |
| `meta` | any | 是 |  | 属于内容数据。 |
| `name` | string | 是 |  |  |
| `owner` | [`Owner`](#owner) | 是 |  |  |
| `pinned` | boolean | 是 |  |  |
| `redacted` | boolean | 否 |  |  |
| `reply` | [`Reply`](#reply)? | 否 |  |  |
| `reply_rev` | uint16 | 否 | `0` | 每当 `reply` 改变时递增。`reply` 协议行和电脑排版后发往设备的文本片段都带有这个修订号。设备据此判断收到的片段属于当前回复还是上一版回复。 |
| `session` | [`SessionInfo`](#sessioninfo) | 是 |  |  |
| `since` | [`Ts`](#ts) | 是 |  | 进入 `state` 的时间。 |
| `slot` | [`Slot`](#slot)? | 否 |  | 该助手在设备上分配到的槽位；`None` 表示当前未分配槽位。 |
| `source` | [`Source`](#source) | 是 |  |  |
| `state` | [`AgentState`](#agentstate) | 是 |  |  |
| `subagents` | uint16 | 否 |  | 会话当前运行的子助手（来自 Claude 的 `SubagentStart` / `SubagentStop`，或所属应用通过 `agent.update {subagents}` 上报的内容）。列表中的数量还包括运行中的工作流。 |
| `task` | string? | 否 |  | 会话的主要任务，也是列表标题：优先使用 CLI 上报的会话标题；其次使用会话的第一条指令（不是最新一条）；否则使用所属应用通过 `agent.update {task}` 设置的值。 |
| `task_source` | [`TitleSource`](#titlesource)? | 否 |  | `task` 的来源；从指令生成的任务名称属于内容数据。 |
| `title` | string? | 否 |  | 当 `title_source` 为 `prompt`/`tool` 时，属于内容数据。 |
| `title_source` | [`TitleSource`](#titlesource) | 是 |  |  |
| `voice` | string? | 否 |  |  |
| `workflow` | [`Plan`](#plan)? | 否 |  |  |
| `workflow_runs` | [`Run`](#run)[] | 否 |  |  |

### `AgentId`

`"ag_…"`：在助手的整个生命周期内保持稳定。

类型：string。


### `AgentRegister`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `caps` | [`Caps`](#caps) | 是 |  |  |
| `cwd` | string? | 否 | `null` |  |
| `key` | string? | 否 | `null` |  |
| `kind` | [`Kind`](#kind) | 是 |  |  |
| `lease_ttl_s` | uint32 | 否 | `0` | 第 4.6 节中的 `lease_ttl_s:30` 是守护进程的默认值，不要求调用方填写。0（字段缺省时的值）表示使用 `[agents] lease_ttl_s`。 |
| `meta` | any | 否 | `null` |  |
| `name` | string | 是 |  |  |
| `pid` | uint32? | 否 | `null` |  |
| `pid_kind` | [`PidKind`](#pidkind)? | 否 | `null` | `pid` 的含义（第 5 节）：钩子辅助程序沿父进程链查找，若找到 CLI 进程本身则为 `cli`，若找到中间的 `sh -c` 则为 `shell`，未找到 CLI 祖先进程则为 `unknown`。**仅**对 `cli` 执行 `kill(pid, 0)` 存活轮询，因此未设置的值不会被轮询。 |
| `slot_hint` | [`Slot`](#slot)? | 否 | `null` |  |
| `terminal` | [`Terminal`](#terminal)? | 否 | `null` |  |
| `voice` | string? | 否 | `null` |  |

### `AgentState`

可选值：`working`、`question`、`ready`、`error`、`idle`。


### `AgentUpdate`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | 是 |  |  |
| `meta` | any | 否 |  |  |
| `name` | string? | 否 |  |  |
| `since` | [`Ts`](#ts)? | 否 |  |  |
| `state` | [`AgentState`](#agentstate)? | 否 |  |  |
| `subagents` | uint16? | 否 |  | 会话当前运行的子助手数量。 |
| `task` | string? | 否 |  | 会话的主要任务（列表标题）；`""` 可将其清空。 |
| `title` | string? | 否 |  |  |
| `title_source` | [`TitleSource`](#titlesource)? | 否 |  |  |

### `AnswerCap`

- string
- string — 仅供适配器使用。


### `Binding`

可选值：`left`、`right`、`enter`、`cw`、`ccw`、`shift+left`、`shift+right`、`shift+enter`。


### `Caps`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `answer` | [`AnswerCap`](#answercap) | 是 |  |  |
| `focus` | boolean | 否 | `false` | 第 4.15 节仅以 `caps:{answer,order}` 注册；助手未声明支持聚焦，就不能对其执行聚焦操作。 |
| `order` | [`OrderCap`](#ordercap) | 是 |  |  |
| `transcribe` | [`TranscribeCap`](#transcribecap) | 否 |  | 助手的所属应用可以转写发给它的语音。设为 `push` 时，面向此助手的录音会保存为 WAV 并交给所属连接（`agent.transcribe`），其返回文字作为最终转写结果。守护进程自身的识别器同时运行；所属应用失败、拒绝或超时时，使用本地识别结果。旧版守护进程会忽略此字段，因此配套应用可以始终发送它。 |

### `Card`

第 4.8 节的卡片状态机。

- {status: string}
- {choice?: uint8?, status: string, text?: string?}
- {status: string}
- {status: string}
- {reason: string, status: string}
- {status: string}
- {focused_at?: [`Ts`](#ts)?, status: string}


### `CardClear`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | 是 |  |  |
| `card` | [`CardId`](#cardid) | 是 |  |  |
| `delivered` | boolean | 是 |  |  |
| `reason` | string? | 否 | `null` |  |

### `CardId`

每个槽位内的卡片 ID，按 `last + 1` 递增，回绕时跳过 0（第 6.5 节）。默认值 0 表示“还没有卡片”，不会出现在传输数据中。

类型：uint16。


### `CardKind`

可选值：`q`、`r`。


### `CardSet`

`agent.card.set`.

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | 是 |  |  |
| `choices` | [`Choice`](#choice)[] | 否 | `[]` |  |
| `deliver` | [`Deliver`](#deliver) | 是 |  |  |
| `free_text` | boolean | 否 | `true` |  |
| `id` | [`CardId`](#cardid)? | 否 |  | 以现有 ID 重新推送：更新同 ID 的内容，不再播放提示音。 |
| `origin` | [`Origin`](#origin) | 否 | `{"group":null,"kind":"custom","request_id":null,"tool":null}` | 第 4.6 节：通过任意令牌或脚本提交的卡片，除非声明了自身来源，否则都标为 `custom`（对讲机上加前缀 `» `）。 |
| `rec` | uint8? | 否 | `null` |  |
| `text` | string | 是 |  |  |
| `timeout_s` | uint32? | 否 | `null` |  |
| `type` | [`CardKind`](#cardkind) | 是 |  |  |

### `ChatImage`

仅保存小型引用。图像字节不会进入快照或事件环形缓冲区。

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `height` | uint32 | 是 |  |  |
| `id` | string | 是 |  |  |
| `label` | string | 是 |  |  |
| `width` | uint32 | 是 |  |  |

### `ChatMessage`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `activity` | [`ActivityStatus`](#activitystatus)? | 否 |  |  |
| `at` | [`Ts`](#ts) | 是 |  |  |
| `id` | string | 是 |  |  |
| `images` | [`ChatImage`](#chatimage)[] | 否 |  |  |
| `plan` | [`Plan`](#plan)? | 否 |  | 显式的公开计划更新；普通聊天或工具消息中缺省。 |
| `role` | [`ChatRole`](#chatrole) | 是 |  |  |
| `runs` | array? | 否 |  | 公开的后台工作流计数，独立于父任务的当前轮次。 |
| `streaming` | boolean | 否 | `false` |  |
| `summary` | [`ActivitySummary`](#activitysummary)? | 否 |  | 原生接入应用提供的公开活动标签或预览。 |
| `text` | string | 是 |  |  |

### `ChatRole`

可选值：`user`、`assistant`、`tool`。


### `Choice`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `detail` | string? | 否 |  |  |
| `label` | string | 是 |  |  |

### `CompanionProfile`

由客户端拥有的分组。标识符决定哪些溢出会话可以调入本组的设备槽位，标签会公开显示。

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `app_id` | string | 是 |  | 稳定的应用标识符，包含 1–64 个 ASCII 字母、数字、点、短横线或下划线。 |
| `group_id` | string | 是 |  | 此应用内稳定的账号或工作区标识符，1–128 字节。请勿放入敏感凭据。 |
| `group_label` | string | 是 |  | 列表标题，1–24 个 UTF-8 字节；不同设备页面应使用不同标题。 |
| `role` | [`CompanionRole`](#companionrole) | 是 |  |  |

### `CompanionRole`

可选值：`session`、`launcher`。


### `CompanionSet`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | 是 |  |  |
| `profile` | [`CompanionProfile`](#companionprofile)? | 否 |  | 传入 null 会移除原生应用的显示配置。已有注册仍然有效。 |

### `Deliver`

- string
- string — 仅供适配器使用：暂存的钩子响应。
- string — 按 ENTER 将会话所属应用切到前台（交接）；用于降级的等待状态和信息卡片。


### `DeviceSet`

第 4.10 节将两个字段命名为 `k` 和 `v`，与传输中的 `set` 协议行和 `device.set` 事件一致。

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `k` | [`SetKey`](#setkey) | 是 |  |  |
| `v` | uint16 | 是 |  |  |

### `DeviceState`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `battery` | uint8? | 否 |  | 对讲机最近上报的电量百分比。未连接或设备不知道电量时为 `None`；设备以 0 表示未知。 |
| `board` | string? | 否 |  | 对讲机上报的电路板名称（`Rev A`、`Rev B`、`LilyGo`）。 |
| `boot` | boolean? | 否 |  |  |
| `charging` | boolean? | 否 |  | 对讲机最近上报的电池是否正在充电。 |
| `deck` | string? | 否 |  | 当前快照中页面列表、设置和宏键盘按键所对应的对讲机（已连接的设备，否则为最后识别的设备）。使用不携带地址信息的标识：8 位十六进制数字，同一台设备每次保持不变，绝不是设备地址。每台设备各自独立；切换到另一台设备的数据时，由 `device.profile` 通知。尚未识别设备时缺省。 |
| `drops` | [`LinkDrop`](#linkdrop)[] | 否 |  | 最近的掉线记录，最早的排在前面；最多 60 条，最旧一条与最新一条相隔不超过一小时。重启后不保留。 |
| `err_counts` | [`ErrCounts`](#errcounts) | 是 |  |  |
| `face` | uint8? | 否 |  | 收到第一个 `face` 事件前为未知。 |
| `faces` | [`FaceEntry`](#faceentry)[] | 是 |  | 设备上报的页面列表。 |
| `fw` | string? | 否 |  |  |
| `host_online` | boolean | 是 |  |  |
| `kinds` | uint8? | 否 |  | 已连接设备能够绘制的协议助手类型数量（其 `hello` 中的 `kinds`）。未连接或设备固件早于此字段时为 `None`。设备不能绘制的助手类型会以 `other` 发送（`Kind::for_deck`）。 |
| `language_supported` | boolean | 否 | `false` |  |
| `last` | [`LastLinked`](#lastlinked)? | 否 |  | 最后在此建立有效连接的对讲机（连接期间即当前设备）；重启后保留。 |
| `link` | [`Link`](#link) | 是 |  |  |
| `outbox_depth` | uint32 | 是 |  |  |
| `pace_ms` | uint16 | 是 |  |  |
| `session` | [`SessionState`](#sessionstate) | 是 |  |  |
| `sessions` | uint8? | 否 |  | 设备可容纳的会话数量（其 `hello` 中的 `agents`）。设备尚未报告时为 `None`；早于此字段的设备容量为 8。 |
| `settings` | [`Settings`](#settings) | 是 |  | 最近上报或推送的设备设置。 |
| `sid` | uint32 | 是 |  | 连接使用的会话 ID：由此守护进程启动时选定（告知 `Store::new` 前为 0）。换成不同于上次连接的设备时，会重新生成。 |
| `transport` | [`Transport`](#transport)? | 否 |  | 已连接设备使用的总线；未连接时为 `None`。 |

### `DormancyState`

服务的后台采集状态（`service.dormancy`）。连续 `window_s` 秒没有设备连接时，服务暂停轮询助手会话、监看对话记录和读取用量。设备一连上就恢复采集。

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `collecting` | boolean | 是 |  | 采集器当前是否运行。 |
| `dormant_since` | [`Ts`](#ts)? | 否 |  | 停止采集的时间，即对讲机最后一次处于连接状态的时刻加上断开连接后继续采集的时长。采集器运行时，以及服务从未连接过对讲机时，为 `null`。 |
| `window_s` | uint32 | 是 |  | 从对讲机最后一次处于连接状态起，采集器继续运行的时长，单位为秒。 |

### `ErrCounts`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `parse` | uint32 | 是 |  |  |
| `range` | uint32 | 是 |  |  |
| `size` | uint32 | 是 |  |  |
| `unknown` | uint32 | 是 |  |  |

### `EventKind`

第 4.12 节的事件类型，序列化为以点分隔的名称。

- string
- string — 对讲机的传输方式、电量、充电状态、电路板、最后连接或掉线记录发生变化（`device.status`）。每个事件都携带全部六个键：未知值为 null，没有掉线记录为 `[]`。
- string — 对讲机自身的按键编辑器修改了一个槽位（`device.macro.edit`）。
- string — 通过 `device.macro.set` 或设备编辑器修改了宏键盘页面的按键（`device.macros`）。`{f, src, keys}` 包含完整布局，与当前 `snapshot.macros[f]` 一致。
- string — 另一台对讲机已连接，服务为它保存的页面列表、设置和宏键盘按键已成为当前快照（`device.profile`）。`{deck, faces, settings, macros}` 各项均为完整数据，与当前 `device.deck`、`faces`、`settings`、`macros` 一致。语言跟随 Mac，由所有设备共用，不随设备切换。
- string — 因十分钟未连接对讲机而停止采集，或已恢复采集（`service.dormancy`）。值为 `{window_s, collecting, dormant_since}`；不支持休眠的服务为 `null`。


### `FaceEntry`

页面列表中的一个条目：界面 ID `f`（1..=8）、模板 `t`、语音操作所用的页面名称 `n`。

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `f` | uint8 | 是 |  |  |
| `n` | string | 是 |  |  |
| `t` | [`Template`](#template) | 是 |  |  |

### `Focus`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid)? | 否 |  |  |
| `seq` | uint16? | 否 |  | 最近的设备 `channel.seq`。 |
| `source` | [`FocusSource`](#focussource)? | 否 |  |  |

### `FocusReport`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid)? | 否 |  |  |
| `source` | [`FocusSource`](#focussource) | 是 |  |  |

### `FocusSource`

可选值：`device`、`desktop`、`api`。


### `Group`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `count` | uint8 | 是 |  |  |
| `index` | uint8 | 是 |  |  |

### `Kind`

可选值：`claude`、`codex`、`gemini`、`other`、`conol`、`grok`。


### `LastLinked`

保存在 `state.json` 中的设备上次连接状态。设备不在身边时，即使服务重启，也能说明上次何时连接、当时运行的版本。

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `at` | [`Ts`](#ts) | 是 |  | 连接仍有效时，表示建立有效连接的时间；断开后，表示结束时间。 |
| `board` | string? | 否 |  |  |
| `fw` | string? | 否 |  |  |
| `transport` | [`Transport`](#transport)? | 否 |  |  |

### `Link`

可选值：`offline`、`connected`、`degraded`。


### `LinkDrop`

一次掉线：记录发生时间及 HID 线程给出的原因（`read`、`write`、`write_stall`、`removed`、`misses`）。

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `at` | [`Ts`](#ts) | 是 |  |  |
| `why` | string | 是 |  |  |

### `Listener`

守护进程无法占用 TCP 端口时，尝试查明占用者（第 4.1 节的 `lsof -nP -iTCP:<port> -sTCP:LISTEN` 查询，尽力获取）。

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `command` | string? | 否 |  |  |
| `pid` | uint64 | 是 |  |  |

### `MacroKey`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `binding` | [`Binding`](#binding)? | 否 |  | 与此宏键绑定的设备实体控件。`None` 表示没有绑定实体控件，只能点按屏幕触发；已配置的快捷键仍会照常发送。 |
| `keys` | string? | 否 |  | 按下此键时守护进程发送的快捷键：`cmd+shift+4`（`keystroke_valid`）。`None` 表示只发送事件。 |
| `label` | string | 是 |  |  |

### `MacroLayout`

主机保存的宏键盘页面按键，由 `device.macro.set` 或设备编辑器设置，并持久保存在 `state.json`，重启后可恢复。对讲机只上报单项修改，不上报整套按键，因此这是唯一的完整副本。

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `keys` | [`MacroKey`](#macrokey)[] | 是 |  | 从槽位 0 开始，最多九个。空标签表示空槽位，后面的按键仍保持原位置。 |
| `src` | string? | 否 |  | 最后设置的宏键盘页头标签；未设置时为 `None`。 |

### `MessageMeta`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `activity` | [`ActivityStatus`](#activitystatus)? | 否 |  |  |
| `id` | string | 是 |  |  |
| `images` | [`ChatImage`](#chatimage)[] | 否 |  |  |
| `plan` | [`Plan`](#plan)? | 否 |  | 显式的公开计划更新；普通聊天或工具消息中缺省。 |
| `role` | [`ChatRole`](#chatrole) | 是 |  |  |
| `runs` | array? | 否 |  | 公开的后台工作流计数，独立于父任务的当前轮次。 |
| `streaming` | boolean | 否 | `false` |  |
| `summary` | [`ActivitySummary`](#activitysummary)? | 否 |  | 原生接入应用提供的公开活动标签或预览。 |

### `ModelOption`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `default_effort` | string | 否 | `""` |  |
| `efforts` | string[] | 否 | `[]` |  |
| `id` | string | 是 |  |  |
| `label` | string | 是 |  |  |

### `OpAnswer`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | 是 |  |  |
| `card` | [`CardId`](#cardid) | 是 |  |  |
| `choice` | uint8? | 否 | `null` |  |
| `text` | string? | 否 | `null` |  |

### `OpOrder`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | 是 |  |  |
| `ctx` | string | 是 |  |  |
| `source` | [`OrderSource`](#ordersource) | 是 |  |  |
| `text` | string | 是 |  |  |
| `utterance` | string? | 否 | `null` | 本条口述指令所对应的 PTT 录音 ID（B12）。第 4.12 节为 `agent.order` 定义了 `utterance?`，而 [`crate::Order`] 自第一周就有此字段，但此前二者都无法填入：产生指令的请求没有位置存放此 ID。因此，客户端只能按文字将指令与来源 `transcript.final` 对应，而这些文字恰好可能被第 5.6 节的语法改写。手动输入的指令（`source:"gui"`、`"api"`）中缺省；使用 `default`，已有调用方不必发送它。 |

### `OrderCap`

- string
- string — 仅供适配器使用。


### `OrderOutcome`

守护进程交给助手所属应用的指令，其最终投递结果。`op.order` 发出的每条指令，无论口述还是键入，都会在 `agent.order` 通知及 `agent.wait` 的 `{kind:"order"}` 结果中携带 `order` ID。“已完成”卡片上选中的步骤（`ctx:"step"`）不带此 ID，对讲机会自行恢复其状态。如果所属应用无法把指令送入对话，应以 `ok: false` 和 `reason` 上报一次，设备会显示未送达。`ok: true` 会被接受，但不改变状态。只有助手所属的 API 连接会话可以上报，其他会话被拒绝并返回 `-32004`；指令必须在最近十分钟内且尚未上报，否则返回 `-32003`；`reason` 必须符合下述单词格式，否则返回 `-32009`。结果为 `{rev, shown}`，显示方式为 `card`、`toast` 或 `none`，表示向设备发送了何种提示。

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | 是 |  | 这条指令的目标助手。 |
| `ok` | boolean | 是 |  | 指令是否到达对话。 |
| `order` | string | 是 |  | 指令携带的 `order` ID。 |
| `reason` | string? | 否 | `null` | 未送达的原因：一个仅含小写字母、数字和下划线的标记，最多 32 字节（如 `backend_refused`）。缺省时为 `undeliverable`。 |

### `OrderSource`

可选值：`ptt`、`gui`、`api`。


### `Origin`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `group` | [`Group`](#group)? | 否 |  |  |
| `kind` | [`OriginKind`](#originkind) | 是 |  |  |
| `request_id` | string? | 否 |  |  |
| `tool` | string? | 否 |  |  |

### `OriginKind`

- string
- string — 仅供内置适配器使用。
- string — 任意令牌或脚本；设备上显示前缀 `» `。


### `Owner`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `lease_expires` | [`Ts`](#ts)? | 否 |  |  |
| `principal` | [`Principal`](#principal) | 是 |  |  |
| `session` | [`SessionId`](#sessionid) | 是 |  |  |

### `Page`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `count` | uint8 | 是 |  |  |
| `index` | uint8 | 是 |  |  |

### `Permission`

可选值：`unknown`、`not_determined`、`granted`、`denied`。


### `Permissions`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `bluetooth` | [`Permission`](#permission) | 否 | `"unknown"` | BLE 语音连接最近读取到的本进程 CoreBluetooth 授权状态（`[voice] transport = "ble"`）。在此传输方式下，设备的麦克风和扬声器需要此权限，而不是麦克风权限。使用 CoreAudio 或连接尚未启动时为 `unknown`。 |
| `microphone` | [`Permission`](#permission) | 是 |  |  |
| `speech` | [`Permission`](#permission) | 是 |  |  |

### `PidKind`

可选值：`cli`、`shell`、`unknown`。


### `Plan`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `explanation` | string? | 否 |  |  |
| `steps` | [`PlanStep`](#planstep)[] | 是 |  |  |

### `PlanStep`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `status` | [`StepStatus`](#stepstatus) | 是 |  |  |
| `step` | string | 是 |  |  |

### `Principal`

类型：string。


### `PttCtx`

可选值：`talk`、`answer`、`name`。


### `PttInfo`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid)? | 否 |  |  |
| `card` | [`CardId`](#cardid)? | 否 |  |  |
| `companion` | boolean | 否 |  | 由助手所属应用转写本次录音（`caps.transcribe`）；本地识别器仅作后备，因此其临时结果不会显示在设备上。 |
| `ctx` | [`PttCtx`](#pttctx) | 是 |  |  |
| `live` | boolean | 否 | `false` | 录音是否已产生**非零采样值**，用来区分第 7.4 节的 `Arming`（false）与 `Listening`（true）。这关系到用户的一秒钟时间，并非小细节：此硬件的 SCO 建连耗时中位数为 1031 ms（V1，测于 2026-09-21）。期间虽然持续收到回调，`AudioUnitRender` 也返回 `noErr`，但所有采样值都严格为数字零。若界面在这一秒显示“正在聆听”，就会误导用户；若仅凭回调判断麦克风存活，麦克风失效时也完全检测不出来（V2）。 |
| `principal` | [`Principal`](#principal) | 是 |  | 谁发起了本次会话：由设备 `ptt` 状态变化触发时为 `device`，否则为 `voice.listen` 的调用方（第 7.4 节）。 |
| `session` | [`SessionId`](#sessionid)? | 否 | `null` | 调用方所在的 **API 连接会话**，仅凭 `principal` 无法确定（B2）。第 4.9 节要求派生出的每个 `op.answer`/`op.order` 同时携带两者。此守护进程按会话而不是访问主体确定归属（第 4.2 节）；缺少此值时，口述答案无法归属到发起请求的连接，所属会话的 `agent.wait` 也无法收到结果。设备触发时为 `None`，因为按键操作不属于任何 API 连接会话。 |
| `since` | [`Ts`](#ts) | 是 |  |  |
| `utterance` | string | 否 | `""` | 本次会话每条转录携带的 ID（`ut_<n>`、第 4.12 节的 `transcript.partial\|final{utterance}` 和 B12 的 `order.dispatched`）。开始录音时即分配，因为临时转录结果会先于其他命名机会到达。 |

### `Reply`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `announce` | string? | 否 |  |  |
| `at` | [`Ts`](#ts) | 是 |  |  |
| `redacted` | boolean | 否 |  |  |
| `text` | string? | 否 |  | 属于内容数据。 |

### `ReplySet`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | 是 |  |  |
| `announce` | string? | 否 | `null` |  |
| `lane` | string? | 否 | `null` |  |
| `message` | [`MessageMeta`](#messagemeta)? | 否 | `null` | 流式回复中，使用服务商提供的稳定条目 ID 持续更新同一条消息。 |
| `speak` | boolean | 否 | `false` |  |
| `text` | string | 是 |  |  |

### `Route`

第 7.2 节的输出路由状态机。B7：原来使用 `Sco`，而第 7.2 节称为 **`ScoOurs`**，且完全没有 `Reopening`。因此，每次录音后约两秒半的状态（V1：A2DP 恢复耗时**中位数为 2307 ms**）无法命名，`voice.route` 也无法报告。区分 `ScoOurs` 正是此状态机的关键：由*本程序*打开麦克风导致的 SCO，松开按键时即可退出；其他应用导致的 SCO 则只能等待结束（V7：此时对讲机是系统默认输入，所以这是常规情况，不是边缘情况）。

- string
- string — 输出以 44.1 kHz 打开并保持开启（第 7.2 节）。
- string — 本程序进行 PTT 录音：暂停音频通道，关闭输出。
- string — 其他应用录音，导致底层采样率变化。
- string — 处于本程序录音结束与 A2DP 恢复之间。V3：立即以设备**当前**标称采样率打开，首次渲染回调为 48 ms；若等待恢复到 44100，则需 2591 ms。每次 PTT 后播报的差距达 54 倍。那 2.3 秒来自 macOS 固定的 SCO 拆除计时器，无法加快。
- string — 两次重新打开失败（第 7.2 节）：以不匹配的采样率打开输出并重采样，报告 `voice.route{stuck:true}`，在下次采样率变化时恢复。
- string — `[voice] fallback_output = "system_default"`：使用的不是对讲机。
- string — BLE 语音连接（`[voice] transport = "ble"`）：设备扬声器特征通道以 16 kHz 打开并保持开启。这是此传输方式的稳定状态，相当于经典蓝牙设备的 `A2dp`。
- string — 本程序通过 BLE 语音连接进行 PTT 录音：按住期间关闭设备扬声器（半双工，无回声消除，V5），松开后重新打开，相当于 BLE 传输中的 `ScoOurs`。


### `Run`

服务提供方上报的后台运行任务。数量统计的是实际启动的工作助手（worker），不根据计划步骤推算。

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `completed` | uint32 | 是 |  |  |
| `elapsed_ms` | uint64 | 是 |  |  |
| `id` | string | 是 |  |  |
| `name` | string | 是 |  |  |
| `status` | [`ActivityStatus`](#activitystatus) | 是 |  |  |
| `tokens` | uint64 | 是 |  |  |
| `total` | uint32 | 是 |  |  |

### `ServiceState`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `degraded` | string? | 否 |  |  |
| `dormancy` | [`DormancyState`](#dormancystate)? | 否 | `null` | 采集器休眠状态；不支持此功能的服务（以 `--no-device` 启动，或版本早于此功能）为 `null`。 |
| `listener` | [`Listener`](#listener)? | 否 | `null` | 当 `degraded` 为 `port_in_use`/`hooks_hijack_risk` 时设置，表示占用端口的进程，用于 `service.degraded` 和 `service.info`（第 4.1、4.12 节）。 |
| `port` | uint16 | 是 |  |  |
| `version` | string | 是 |  |  |

### `SessionControls`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `editable` | boolean | 否 | `false` |  |
| `effort` | string | 是 |  |  |
| `model` | string | 是 |  |  |
| `models` | [`ModelOption`](#modeloption)[] | 否 | `[]` |  |
| `note` | string | 否 | `""` |  |

### `SessionId`

类型：string。


### `SessionInfo`

助手运行的位置（除 `key` 外均属于内容数据）。

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `cwd` | string? | 否 |  |  |
| `key` | string? | 否 |  | `"claude:<session_id>"`、`"myapp:<id>"`：幂等键。 |
| `pid` | uint32? | 否 |  |  |
| `pid_kind` | [`PidKind`](#pidkind)? | 否 |  |  |
| `terminal` | [`Terminal`](#terminal)? | 否 |  |  |

### `SessionSet`

经过归属验证的设置报告。普通刷新时省略 `request_rev`。

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | 是 |  |  |
| `controls` | [`SessionControls`](#sessioncontrols) | 是 |  |  |
| `request_rev` | uint16? | 否 | `null` | 应用设置后，确认所处理的确切 `session.settings_requested` 修订号。如果更改失败，上报实际设置和一条非空说明。 |

### `SessionState`

§6.3.

可选值：`absent`、`linked`、`await_hello`、`syncing`、`live`、`degraded`。


### `SetKey`

- string
- string — API 设置；通过带版本号的 `language` 消息传给设备。
- string — 对讲机显示收到的文档时使用的字号：0 小、1 中、2 大。**主机**必须知道此值，因为文字由主机换行；两端字号不一致会导致每行的换行位置都出错。


### `Settings`

主机持久保存并在 `boot:1` 时恢复的 `set` 键。

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `announce` | uint16 | 否 | `1` |  |
| `arcadesound` | uint16 | 否 | `0` | 0 关闭，1 仅音效，2 音乐和音效；在小游戏菜单中修改。 |
| `autojump` | uint16 | 否 | `0` |  |
| `brightness` | uint16 | 否 | `5` |  |
| `chime` | uint16 | 否 | `1` |  |
| `demo` | uint16 | 否 | `0` |  |
| `idlehome` | uint16 | 否 | `120` |  |
| `language` | uint16 | 否 | `0` | 与支持此功能的设备共用的界面语言：en、zh-Hans、zh-Hant、ja。 |
| `language_revision` | uint32 | 否 | `0` |  |
| `mic` | uint16 | 否 | `6` |  |
| `speaker` | uint16 | 否 | `6` |  |
| `textsize` | uint16 | 否 | `1` | 0 小、1 中、2 大。**电脑端**会按选定字号对应的字体排版、换行，因此两端都需要这个设置。 |

### `Slot`

设备槽位为 1..=255（传输时占一个字节）。设备容量为 `DeviceState::capacity`；未报告容量的设备默认为 8。

类型：uint8。


### `Source`

可选值：`hook`、`api`、`mcp`。


### `Speaking`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid)? | 否 |  |  |
| `lane` | string | 是 |  |  |
| `utterance` | string | 是 |  |  |

### `StepStatus`

可选值：`pending`、`in_progress`、`completed`。


### `SttInfo`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `engine` | string | 是 |  |  |
| `locale` | string | 是 |  |  |
| `on_device` | boolean | 是 |  |  |

### `Template`

可选值：`agents`、`omarchy`、`macropad`、`usage`、`settings`、`arcade`、`system`、`timer`。


### `Terminal`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `app` | string? | 否 |  | 钩子辅助程序找到的会话所属应用的 macOS bundle 标识符（`dev.zed.Zed`、`com.mitchellh.ghostty`、`com.anthropic.claudefordesktop`）。按会话的 `Open app` 卡片会将应用切到前台。对于由已连接应用注册的助手，此字段仅作信息展示；守护进程只会唤起通过钩子注册的会话所属应用。 |
| `inbox` | string? | 否 |  | 会话自己的消息收件箱，即 Claude Code 为跨会话消息绑定的 Unix 套接字（`CLAUDE_CODE_MESSAGING_SOCKET`），从钩子辅助程序环境中获取。具备此收件箱的助手通过向其中投递消息接收指令，不再向其终端模拟输入（第 5.5 节），因此不需要焦点、AppleScript 或 TCC 授权。与之配套的会话令牌**不在这里**，它只保留在守护进程内部（`adapters::rt::Adapters`），绝不进入快照。 |
| `iterm_session` | string? | 否 |  |  |
| `program` | string? | 否 |  |  |
| `term_session` | string? | 否 |  |  |
| `title_stamp` | string? | 否 |  | 移除 Ghostty 标签页路径后不再写入；保留此字段，以便仍携带它的旧客户端请求可以正常解析。 |
| `tmux_pane` | string? | 否 |  |  |
| `tty` | string? | 否 |  |  |

### `TitleSource`

- string — `session_name` / `name`：属于状态数据。
- string — 来自当前工作目录：属于状态数据。
- string — 用户指令的前 64 字节：属于内容数据。
- string — 工具输入的开头：属于内容数据。
- string — 通过 `agent.update` 设置：属于状态数据。


### `Tone`

可选值：`blue`、`amber`、`green`、`red`、`gray`。


### `TranscribeCap`

谁有权将本次录音转成文字（`caps.transcribe`）。

- string — 所属应用会收到面向此助手的每段语音的 `agent.transcribe`，并以 `agent.transcribe.result` 回复。
- string — 仅使用守护进程自身的识别器。


### `Transport`

对讲机与本电脑的连接方式，即 HID 线程打开设备时使用的总线。

可选值：`usb`、`bluetooth`。


### `Ts`

类型：string。


### `TtsInfo`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `engine` | string | 是 |  |  |

### `Usage`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `at` | [`Ts`](#ts)? | 否 |  |  |
| `items` | [`UsageItem`](#usageitem)[] | 是 |  |  |
| `period` | string | 是 |  |  |

### `UsageItem`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `account` | string? | 否 |  | 这些用量统计周期对应的账号；仅在数据源提供账号信息时填写。 |
| `name` | string | 是 |  |  |
| `percent` | uint8 | 是 |  | 0..=100. |
| `reset` | string | 是 |  |  |
| `reset_tip` | string? | 否 |  | 即将到期、尚可使用的重置机会提醒；没有待提醒项时缺省。 |
| `tone` | [`Tone`](#tone)? | 否 |  | `None` 表示根据 `percent` 决定进度条颜色。 |

### `VoiceInput`

PTT 会话打开哪个麦克风（第 4.9 节 `voice.listen{input}`）。

- string — 设备自身的输入 UID；设备 `ptt` 状态变化始终只指向它。
- string — Mac 默认输入设备；只有 `voice.listen{input:"mac"}` 会请求它，设备按键录音绝不会回退到此输入。


### `VoiceListen`

`voice.listen`（第 4.9 节）：第 7.4 节状态机的 GUI/API 入口，与设备 `ptt` 状态变化驱动的是同一套状态机。所有派生操作均使用**调用方**的访问主体和会话，绝不使用 `device`（B2）。

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | 是 |  |  |
| `card` | [`CardId`](#cardid)? | 否 | `null` |  |
| `ctx` | string | 否 | `"talk"` | `"talk"` 或 `"answer"`。使用字符串而不是 [`crate::PttCtx`]，因为第 4.9 节只允许这两种取值；`name` 由设备状态变化触发，API 调用方不得模拟。 |
| `input` | [`VoiceInput`](#voiceinput) | 否 | `"deck"` | `"deck"`（默认）或 `"mac"`，即第 4.9 节的 `input`。 |
| `max_s` | uint32 | 否 | `60` |  |

### `VoiceSay`

`voice.say` (§4.9).

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid)? | 否 | `null` |  |
| `interrupt` | boolean | 否 | `false` | 第 4.9 节：不能抢占更高优先级的音频通道，由混音器决定。 |
| `lane` | string | 是 |  |  |
| `text` | string | 是 |  |  |
| `ttl_s` | uint32? | 否 | `null` | 第 4.9 节的 `ttl_s?:30`：若 `chatter` 到期仍未开始，则丢弃。此值会传给混音器，不会在这里忽略；调用方要求设置期限却被静默忽略，比明确拒绝更糟。 |
| `voice` | string? | 否 | `null` |  |

### `VoiceState`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `input_uid` | string? | 否 |  |  |
| `output_uid` | string? | 否 |  |  |
| `permissions` | [`Permissions`](#permissions) | 是 |  |  |
| `ptt` | [`PttInfo`](#pttinfo)? | 否 |  |  |
| `route` | [`Route`](#route) | 是 |  |  |
| `speaking` | [`Speaking`](#speaking)? | 否 |  |  |
| `stt` | [`SttInfo`](#sttinfo) | 是 |  |  |
| `transport` | [`VoiceTransport`](#voicetransport)? | 否 | `null` | 音频引擎创建时选用的传输方式（`[voice] transport`）。引擎尚未上报时，该值为 null；不含音频引擎的构建也返回 null。 |
| `tts` | [`TtsInfo`](#ttsinfo) | 是 |  |  |

### `VoiceTransport`

对讲机的音频传输方式（`[voice] transport`）：通过 BLE 连接使用设备自己的 GATT 音频服务（`deckd::ble`），或通过 CoreAudio 访问 Rev A 之前的经典蓝牙 HFP/A2DP 设备对。

- string
- string — Rev A 固件已不再是经典蓝牙音频设备，因此默认使用此方式。


### `Window`

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|---|---|---|---|---|
| `title` | string | 是 |  |  |
| `tone` | [`Tone`](#tone)? | 否 |  | 缺省时显示灰色。 |

---

本文件为中文翻译预览，尚未经过母语审阅者逐行审核。接口定义以[英文原文](/docs/deckd-api.md)为准。
