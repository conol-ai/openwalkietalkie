# `deckd` v1 API リファレンス

[English](/docs/deckd-api.md) | **日本語**

<!-- GENERATED FILE — DO NOT EDIT. -->

このリファレンスの英語原文は、動作中のデーモンが `GET /v1/schema` で返す内容から自動生成されます。各メソッド、イベント、型の JSON Schema は `deck-model` の Rust 定義を基にし、メソッドの振り分け表も参照します。英語原文を直接編集しても、`deckd` のテスト `the_checked_in_api_document_is_what_the_schema_produces` が毎回、生成結果と照合します。仕様を変更するときは Rust の型またはメソッド表を変更し、次のコマンドで再生成してください。

```sh
cargo run --locked -p deckd --target aarch64-apple-darwin --target-dir target/host --manifest-path host/Cargo.toml -- api-doc > docs/deckd-api.md
```

トランスポート、呼び出し主体とスコープ、コンテンツの分類、配信保証、エラーコードなど、API の設計理由は [`docs/host-service.md` の第 4 節](https://github.com/conol-ai/openwalkietalkie/blob/main/docs/host-service.md#4-the-open-api) で説明しています。このリファレンスにはスキーマから取得できる情報だけを掲載します。スキーマ外の説明が実装と食い違っても、自動では検出できないためです。

## メソッド

「スコープ」（`scope`）は呼び出し主体に最低限必要なスコープです（§4.2）。スコープ不要のメソッドはハンドシェイク前でも応答します。「応答元」（`answered by`）が `store` の場合はストアのリデューサーがスナップショットを基に応答し、`core` の場合はコアスレッドが別経路で応答します。「パラメーター」（`params`）のリンク先でリクエストの型を確認できます。

| メソッド | スコープ | 応答元 | パラメーター |
|---|---|---|---|
| `session.hello` | —（スコープ不要） | core | —（`Request` 列挙型には含まれません） |
| `session.subscribe` | —（スコープ不要） | core | —（`Request` 列挙型には含まれません） |
| `session.unsubscribe` | —（スコープ不要） | core | —（`Request` 列挙型には含まれません） |
| `session.ping` | —（スコープ不要） | core | —（`Request` 列挙型には含まれません） |
| `state.get` | `observe` | core | —（`Request` 列挙型には含まれません） |
| `state.events` | `observe` | core | —（`Request` 列挙型には含まれません） |
| `agent.list` | `observe` | core | —（`Request` 列挙型には含まれません） |
| `agent.get` | `observe` | core | —（`Request` 列挙型には含まれません） |
| `image.get` | `observe:content` | core | —（`Request` 列挙型には含まれません） |
| `device.get` | `observe` | core | —（`Request` 列挙型には含まれません） |
| `voice.get` | `observe` | core | —（`Request` 列挙型には含まれません） |
| `service.info` | `observe` | core | —（`Request` 列挙型には含まれません） |
| `sources.list` | `observe` | core | —（`Request` 列挙型には含まれません） |
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
| `agent.wait` | `agent` | core | —（`Request` 列挙型には含まれません） |
| `agent.order.result` | `agent` | store | [`OrderOutcome`](#orderoutcome) |
| `agent.transcribe.audio` | `agent` | core | —（`Request` 列挙型には含まれません） |
| `agent.transcribe.result` | `agent` | core | —（`Request` 列挙型には含まれません） |
| `op.answer` | `operate` | store | [`OpAnswer`](#opanswer) |
| `op.order` | `operate` | store | [`OpOrder`](#oporder) |
| `op.focus` | `operate` | store | {agent: [`AgentId`](#agentid)} |
| `op.snooze` | `operate` | store | {agent: [`AgentId`](#agentid), card: [`CardId`](#cardid)} |
| `op.ack` | `operate` | store | {agent: [`AgentId`](#agentid), card: [`CardId`](#cardid)} |
| `voice.say` | `voice` | store | [`VoiceSay`](#voicesay) |
| `voice.stop` | `voice` | store | {all?: boolean, lane?: string?, utterance?: string?} |
| `voice.listen` | `voice` | store | [`VoiceListen`](#voicelisten) |
| `voice.cancel` | `voice` | store | {} |
| `voice.engines` | `voice` | core | —（`Request` 列挙型には含まれません） |
| `voice.test` | `voice` | core | —（`Request` 列挙型には含まれません） |
| `firmware.status` | `observe` | core | —（`Request` 列挙型には含まれません） |
| `firmware.check` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `firmware.install` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `stt.status` | `voice` | core | —（`Request` 列挙型には含まれません） |
| `stt.install` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `stt.remove` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `tts.status` | `voice` | core | —（`Request` 列挙型には含まれません） |
| `tts.install` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `tts.remove` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `perm.status` | `voice` | core | —（`Request` 列挙型には含まれません） |
| `perm.request` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `device.set` | `device` | store | [`DeviceSet`](#deviceset) |
| `device.faces.set` | `device` | store | {list: [`FaceEntry`](#faceentry)[]} |
| `device.usage.set` | `device` | store | {items: [`UsageItem`](#usageitem)[], period: string} |
| `device.macro.set` | `device` | store | {f: uint8, keys: [`MacroKey`](#macrokey)[], layout: string, src?: string?} |
| `device.ws.set` | `device` | store | {active: uint8, count: uint8, sel: uint8, windows: [`Window`](#window)[]} |
| `device.name` | `device` | store | {text: string} |
| `focus.report` | `device` | store | [`FocusReport`](#focusreport) |
| `config.get` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `config.set` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `config.reload` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `token.create` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `token.list` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `token.revoke` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `token.rotate` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `metrics.get` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `integrations.status` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `integrations.install` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `integrations.uninstall` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `integrations.repair` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `care.checks` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `care.footprint` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `care.support_file` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `care.cache.clear` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `care.test.question` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `care.test.voice` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `care.test.say` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `care.test.status` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `care.decks` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `care.decks.forget` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `log.tail` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `device.flood` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `device.sync` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `service.restart` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `service.stop` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `grokbot.say` | `agent` | core | —（`Request` 列挙型には含まれません） |
| `grokbot.ask` | `agent` | core | —（`Request` 列挙型には含まれません） |
| `grokbot.state` | `agent` | core | —（`Request` 列挙型には含まれません） |
| `grokbot.status` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `grokbot.add` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `grokbot.remove` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `grokbot.test` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `context.key.set` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `context.key.clear` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `providers.list` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `providers.models` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `providers.refresh` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `providers.test` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `providers.key.set` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `providers.key.clear` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `providers.add` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `providers.remove` | `admin` | core | —（`Request` 列挙型には含まれません） |
| `hooks.claude` | `hooks:ingest` | core | —（`Request` 列挙型には含まれません） |
| `hooks.codex` | `hooks:ingest` | core | —（`Request` 列挙型には含まれません） |
| `hooks.gemini` | `hooks:ingest` | core | —（`Request` 列挙型には含まれません） |
| `hooks.codex_notify` | `hooks:ingest` | core | —（`Request` 列挙型には含まれません） |
| `hooks.claude_statusline` | `hooks:ingest` | core | —（`Request` 列挙型には含まれません） |

## イベント

サーバーからのプッシュは JSON-RPC 通知 `{"method":"event", "params": Event}` の形式で届きます（§4.12）。配信時に購読者ごとに `data` の一部が伏せられ、各セッションが参照できる内容はそのスコープによって決まります。

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `data` | any | はい |  |  |
| `principal` | [`Principal`](#principal) | はい |  |  |
| `rev` | uint64 | はい |  |  |
| `ts` | [`Ts`](#ts) | はい |  |  |
| `type` | [`EventKind`](#eventkind) | はい |  |  |

`type` は次のいずれかです。


## スナップショット

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `agents` | object | はい |  |  |
| `device` | [`DeviceState`](#devicestate) | はい |  |  |
| `faces` | [`FaceEntry`](#faceentry)[] | はい |  | 最後に適用されたページの並び。`boot:1` で復元します。 |
| `focus` | [`Focus`](#focus) | はい |  |  |
| `macros` | object | いいえ |  | 各マクロパッドのキー設定をページ ID 別に保持します。変更のたびに `device.macros` で通知します。どのページにも設定がない間はフィールド自体を省略するため、このフィールドを知らない旧クライアントに影響しません。 |
| `rev` | uint64 | はい |  | 外部から観測できる変更があったときだけ増加します。 |
| `service` | [`ServiceState`](#servicestate) | はい |  |  |
| `settings` | [`Settings`](#settings) | はい |  | ホストに保存された設定。`boot:1` で復元します。 |
| `usage` | [`Usage`](#usage)? | いいえ |  |  |
| `voice` | [`VoiceState`](#voicestate) | はい |  |  |

## エラー

§4.4 で定義する JSON-RPC エラー（`code`、`message`、`data`）。

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `code` | int32 | はい |  |  |
| `data` | any | はい |  |  |
| `message` | string | はい |  |  |

## 接続イベント

HID スレッドが通知する接続状態の変化（§6.2、§6.3）。

- {link: string} — デバイスを開き、`hello` を送信中です。
- {link: string} — 読み書きのエラー、取り外し、または hello への応答が 6 回なかったため、ハンドルを解放しました。
- {link: string} — hello への応答が 3 回ありません。ハンドルは保持します。
- {link: string, sid: uint32} — 自身の sid と一致する hello 応答を受信し、セッションが有効になりました。
- {boot: boolean, link: string, sid: uint32} — 異なる sid の hello 応答を受信し、同期用のバリアを書き込み中です。
- {link: string, sid: uint32} — バリアの `synced` を書き込みました。
- {link: string} — core→hid チャンネルがあふれたため、コアが現在の状態からデバイス向けの表示を再構成します。
- {link: string, pace_ms: uint16} — 集中送信後に接続が 2 回切れたため、送信間隔（ms）を延ばしました。


## 型

上記のスキーマが参照する 92 個の名前付き型です。

### `ActivityStatus`

次のいずれか。`running`, `completed`, `failed`, `stopped`。


### `ActivitySummary`

表示用のテキストだけを含みます。ツールに渡した引数や実行結果の生データは含めません。

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `detail` | string | いいえ | `""` |  |
| `label` | string | はい |  |  |

### `Agent`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `caps` | [`Caps`](#caps) | はい |  |  |
| `card` | [`Card`](#card)? | いいえ |  |  |
| `chat_rev` | uint16 | いいえ | `0` | メッセージの追加中は変わらず、保持している履歴の一部を削除すると変わります。 |
| `companion` | [`CompanionProfile`](#companionprofile)? | いいえ |  | ネイティブコンパニオン用の任意の情報。グループと、ランチャーまたはセッションとしての識別情報を表します。 |
| `id` | [`AgentId`](#agentid) | はい |  |  |
| `kind` | [`Kind`](#kind) | はい |  |  |
| `messages` | [`ChatMessage`](#chatmessage)[] | いいえ | `[]` | エージェントごとの最近の会話。最大 64 メッセージ、512 KiB までです。 |
| `meta` | any | はい |  | コンテンツに分類されます。 |
| `name` | string | はい |  |  |
| `owner` | [`Owner`](#owner) | はい |  |  |
| `pinned` | boolean | はい |  |  |
| `redacted` | boolean | いいえ |  |  |
| `reply` | [`Reply`](#reply)? | いいえ |  |  |
| `reply_rev` | uint16 | いいえ | `0` | `reply` が変わるたびに増加します。`reply` 行と、ホストがレイアウトした表示範囲の両方にこの値を付けることで、デバイスは現在のテキストの表示範囲と、変更前のテキストの表示範囲を区別できます。 |
| `session` | [`SessionInfo`](#sessioninfo) | はい |  |  |
| `since` | [`Ts`](#ts) | はい |  | `state` に移行した時刻。 |
| `slot` | [`Slot`](#slot)? | いいえ |  | このエージェントに割り当てられたデバイス上のスロット。`None` は「デバイスに表示されていない」ことを示します。 |
| `source` | [`Source`](#source) | はい |  |  |
| `state` | [`AgentState`](#agentstate) | はい |  |  |
| `subagents` | uint16 | いいえ |  | セッション内で実行中のサブエージェント。Claude の `SubagentStart` / `SubagentStop`、または所有者が `agent.update {subagents}` で報告した値を使います。一覧の件数には実行中のワークフローも加算します。 |
| `task` | string? | いいえ |  | 一覧のタイトルとなる、セッションの主なタスク。CLI が報告したセッション名、セッションの最初のプロンプト（最新のものではありません）、所有者が `agent.update {task}` で設定した値の順に採用します。 |
| `task_source` | [`TitleSource`](#titlesource)? | いいえ |  | `task` の取得元。プロンプト由来のタスクはコンテンツとして扱います。 |
| `title` | string? | いいえ |  | `title_source` が `prompt` または `tool` の場合はコンテンツとして扱います。 |
| `title_source` | [`TitleSource`](#titlesource) | はい |  |  |
| `voice` | string? | いいえ |  |  |
| `workflow` | [`Plan`](#plan)? | いいえ |  |  |
| `workflow_runs` | [`Run`](#run)[] | いいえ |  |  |

### `AgentId`

`"ag_…"`。エージェントが存在する間は変わりません。

型は string。


### `AgentRegister`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `caps` | [`Caps`](#caps) | はい |  |  |
| `cwd` | string? | いいえ | `null` |  |
| `key` | string? | いいえ | `null` |  |
| `kind` | [`Kind`](#kind) | はい |  |  |
| `lease_ttl_s` | uint32 | いいえ | `0` | §4.6 の `lease_ttl_s:30` はデーモンの既定値です。呼び出し元が指定する必要はなく、0（省略時の値）なら `[agents] lease_ttl_s` が適用されます。 |
| `meta` | any | いいえ | `null` |  |
| `name` | string | はい |  |  |
| `pid` | uint32? | いいえ | `null` |  |
| `pid_kind` | [`PidKind`](#pidkind)? | いいえ | `null` | `pid` が指すプロセス（§5）。フックヘルパーが親プロセスをたどり、CLI 本体なら `cli`、途中の `sh -c` なら `shell`、祖先に CLI がなければ `unknown` とします。`kill(pid, 0)` による生存確認は `cli`  の場合に **限って** 行い、未設定ならポーリングしません。 |
| `slot_hint` | [`Slot`](#slot)? | いいえ | `null` |  |
| `terminal` | [`Terminal`](#terminal)? | いいえ | `null` |  |
| `voice` | string? | いいえ | `null` |  |

### `AgentState`

次のいずれか。`working`, `question`, `ready`, `error`, `idle`。


### `AgentUpdate`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | はい |  |  |
| `meta` | any | いいえ |  |  |
| `name` | string? | いいえ |  |  |
| `since` | [`Ts`](#ts)? | いいえ |  |  |
| `state` | [`AgentState`](#agentstate)? | いいえ |  |  |
| `subagents` | uint16? | いいえ |  | セッションで現在実行中のサブエージェント数。 |
| `task` | string? | いいえ |  | セッションの主なタスク（一覧のタイトル）。`""` で消去します。 |
| `title` | string? | いいえ |  |  |
| `title_source` | [`TitleSource`](#titlesource)? | いいえ |  |  |

### `AnswerCap`

- string
- string — アダプター専用。


### `Binding`

次のいずれか。`left`, `right`, `enter`, `cw`, `ccw`, `shift+left`, `shift+right`, `shift+enter`。


### `Caps`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `answer` | [`AnswerCap`](#answercap) | はい |  |  |
| `focus` | boolean | いいえ | `false` | §4.15 の登録で指定するのは `caps:{answer,order}` だけです。フォーカス可能と宣言していないエージェントにフォーカスは移せません。 |
| `order` | [`OrderCap`](#ordercap) | はい |  |  |
| `transcribe` | [`TranscribeCap`](#transcribecap) | いいえ |  | エージェントの所有者が、そのエージェント宛ての発話を文字起こしできます。`push` を指定すると、PTT を押している間の音声を WAV で録音し、所有者の接続に渡します（`agent.transcribe`）。所有者が返したテキストが最終結果になります。デーモンの認識エンジンも並行して動作し、所有者が失敗、辞退、または時間切れになった場合はその認識結果を採用します。古いデーモンはこのフィールドを無視するので、コンパニオンは常に送信できます。 |

### `Card`

§4.8 のカードの状態遷移。

- {status: string}
- {choice?: uint8?, status: string, text?: string?}
- {status: string}
- {status: string}
- {reason: string, status: string}
- {status: string}
- {focused_at?: [`Ts`](#ts)?, status: string}


### `CardClear`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | はい |  |  |
| `card` | [`CardId`](#cardid) | はい |  |  |
| `delivered` | boolean | はい |  |  |
| `reason` | string? | いいえ | `null` |  |

### `CardId`

スロットごとのカード ID。`last + 1` とし、オーバーフロー時は 0 を飛ばして先頭に戻ります（§6.5）。既定値 0 は「まだカードがない」を意味し、通信では送信しません。

型は uint16。


### `CardKind`

次のいずれか。`q`, `r`。


### `CardSet`

`agent.card.set`.

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | はい |  |  |
| `choices` | [`Choice`](#choice)[] | いいえ | `[]` |  |
| `deliver` | [`Deliver`](#deliver) | はい |  |  |
| `free_text` | boolean | いいえ | `true` |  |
| `id` | [`CardId`](#cardid)? | いいえ |  | 既存の ID で再送します。同じカードの内容を更新し、通知音は再度鳴らしません。 |
| `origin` | [`Origin`](#origin) | いいえ | `{"group":null,"kind":"custom","request_id":null,"tool":null}` | §4.6。トークンまたはスクリプトからのカードは、発行元を明示しない限り `custom` となり、デバイス上では先頭に `» ` が付きます。 |
| `rec` | uint8? | いいえ | `null` |  |
| `text` | string | はい |  |  |
| `timeout_s` | uint32? | いいえ | `null` |  |
| `type` | [`CardKind`](#cardkind) | はい |  |  |

### `ChatImage`

小さな参照情報のみ。画像のバイト列がスナップショットやイベントのリングバッファに入ることはありません。

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `height` | uint32 | はい |  |  |
| `id` | string | はい |  |  |
| `label` | string | はい |  |  |
| `width` | uint32 | はい |  |  |

### `ChatMessage`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `activity` | [`ActivityStatus`](#activitystatus)? | いいえ |  |  |
| `at` | [`Ts`](#ts) | はい |  |  |
| `id` | string | はい |  |  |
| `images` | [`ChatImage`](#chatimage)[] | いいえ |  |  |
| `plan` | [`Plan`](#plan)? | いいえ |  | 明示的な公開プランの更新。通常のチャットやツールのメッセージでは省略します。 |
| `role` | [`ChatRole`](#chatrole) | はい |  |  |
| `runs` | array? | いいえ |  | 公開されるバックグラウンドワークフローのカウンター。親ターンとは独立しています。 |
| `streaming` | boolean | いいえ | `false` |  |
| `summary` | [`ActivitySummary`](#activitysummary)? | いいえ |  | ネイティブコンパニオンが提供する、公開用のアクティビティ名とプレビュー。 |
| `text` | string | はい |  |  |

### `ChatRole`

次のいずれか。`user`, `assistant`, `tool`。


### `Choice`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `detail` | string? | いいえ |  |  |
| `label` | string | はい |  |  |

### `CompanionProfile`

クライアントが所有するグループ。識別子で空きスロットへの繰り上げ対象を区別します。ラベルは公開情報です。

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `app_id` | string | はい |  | アプリの安定した識別子。ASCII の英数字、ピリオド、ハイフン、アンダースコアで 1〜64 文字です。 |
| `group_id` | string | はい |  | アプリ内のアカウントまたはワークスペースの安定した識別子（1〜128 バイト）。秘密情報は入れないでください。 |
| `group_label` | string | はい |  | 一覧の見出し（UTF-8 で 1〜24 バイト）。デバイス上で別々のページに分ける場合は異なる見出しを使います。 |
| `role` | [`CompanionRole`](#companionrole) | はい |  |  |

### `CompanionRole`

次のいずれか。`session`, `launcher`。


### `CompanionSet`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | はい |  |  |
| `profile` | [`CompanionProfile`](#companionprofile)? | いいえ |  | null を指定するとネイティブ表示を解除します。既存の登録は有効なままです。 |

### `Deliver`

- string
- string — アダプター専用。応答を保留中のフック。
- string — ENTER でセッションのアプリを前面に出し、そこで操作を引き継ぎます。機能制限時の保留や情報カードに使います。


### `DeviceSet`

§4.10 では、通信上の `set` 行および `device.set` イベントに合わせて、2 つのフィールドを `k` と `v` と呼びます。

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `k` | [`SetKey`](#setkey) | はい |  |  |
| `v` | uint16 | はい |  |  |

### `DeviceState`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `battery` | uint8? | いいえ |  | デバイスが最後に報告したバッテリー残量（%）。未接続の場合、およびデバイス側で残量不明の場合（送信値は 0）は `None` です。 |
| `board` | string? | いいえ |  | デバイスが報告する基板名（`Rev A`、`Rev B`、`LilyGo`）。 |
| `boot` | boolean? | いいえ |  |  |
| `charging` | boolean? | いいえ |  | セルに充電電流が流れているかどうか。デバイスからの最新の報告に基づきます。 |
| `deck` | string? | いいえ |  | スナップショットのページ構成、設定、マクロパッドのキーが属するデバイス（接続中のデバイス、または最後に識別されたデバイス）。値は 8 桁の 16 進数からなる識別用ラベルです。同一のデバイスでは一定で、アドレスは含みません。デバイスごとに異なり、別のデバイスの設定に切り替わると `device.profile` で通知します。デバイスを識別するまでは省略します。 |
| `drops` | [`LinkDrop`](#linkdrop)[] | いいえ |  | 最近の切断を古い順に保持します。最大 60 件で、直近の切断から 1 時間を超えて古い記録は除きます。再起動すると消去されます。 |
| `err_counts` | [`ErrCounts`](#errcounts) | はい |  |  |
| `face` | uint8? | いいえ |  | 最初の `face` イベントを受け取るまでは不明です。 |
| `faces` | [`FaceEntry`](#faceentry)[] | はい |  | デバイスが報告したページの並び。 |
| `fw` | string? | いいえ |  |  |
| `host_online` | boolean | はい |  |  |
| `kinds` | uint8? | いいえ |  | 接続中のデバイスが表示できるプロトコル上のエージェント種別の数（`hello` の `kinds`）。未接続の場合や、このフィールドを持たない旧デバイスでは `None` です。対応していない種別のエージェントは `other` として送ります（`Kind::for_deck`）。 |
| `language_supported` | boolean | いいえ | `false` |  |
| `last` | [`LastLinked`](#lastlinked)? | いいえ |  | 最後にここで通信可能になったデバイス（接続中であればそのデバイス）。再起動後も保持します。 |
| `link` | [`Link`](#link) | はい |  |  |
| `outbox_depth` | uint32 | はい |  |  |
| `pace_ms` | uint16 | はい |  |  |
| `session` | [`SessionState`](#sessionstate) | はい |  |  |
| `sessions` | uint8? | いいえ |  | デバイスが保持できるセッション数（`hello` の `agents`）。報告を受けるまでは `None` で、このフィールドのない旧デバイスでは 8 です。 |
| `settings` | [`Settings`](#settings) | はい |  | 最後に報告または送信されたデバイス設定。 |
| `sid` | uint32 | はい |  | 接続に使うセッション ID。デーモンの起動時に選びます（`Store::new` に渡るまでは 0）。前回と異なるデバイスにつなぐたびに新しい ID を割り当てます。 |
| `transport` | [`Transport`](#transport)? | いいえ |  | 接続中のデバイスが使うバス。未接続時は `None` です。 |

### `DormancyState`

サービスの収集休止状態（`service.dormancy`）。デバイス未接続のまま `window_s` 秒たつと、エージェントセッションの収集（セッションのポーリング、会話履歴の監視、使用量の読み取り）を止め、デバイスが接続されるとすぐに再開します。

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `collecting` | boolean | はい |  | 現在、収集処理が動いているかどうか。 |
| `dormant_since` | [`Ts`](#ts)? | いいえ |  | 収集を停止した時刻。デバイスの最終接続時刻に猶予期間を加えた値です。収集中または一度もデバイスが接続されていない場合は `null` です。 |
| `window_s` | uint32 | はい |  | デバイスの最終接続後、収集処理を続ける秒数。 |

### `ErrCounts`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `parse` | uint32 | はい |  |  |
| `range` | uint32 | はい |  |  |
| `size` | uint32 | はい |  |  |
| `unknown` | uint32 | はい |  |  |

### `EventKind`

§4.12 のイベント種別。ドット区切りの名前にシリアライズします。

- string
- string — デバイスの接続方式、バッテリー残量、充電状態、基板、最終接続または切断履歴の変化（`device.status`）。各イベントは 6 つのキーをすべて持ち、不明な値は null、切断なしは `[]` です。
- string — デバイス本体のキーエディターでスロットを変更しました（`device.macro.edit`）。
- string — `device.macro.set` または本体のエディターでマクロパッドのキーが変更されました（`device.macros`）。`{f, src, keys}` は `snapshot.macros[f]` に現在保存されているレイアウト全体です。
- string — 別のデバイスが接続され、サービスに保存してあるそのデバイスのページ構成、設定、マクロパッドのキーがスナップショットに反映されました（`device.profile`）。`{deck, faces, settings, macros}` は、それぞれ `device.deck`、`faces`、`settings`、`macros` が保持する内容の全体です。言語は Mac の設定をすべてのデバイスで共有し、デバイスを替えても変わりません。
- string — デバイス未接続のまま 10 分経過して収集処理が停止した、または収集が再開したことを通知します（`service.dormancy`）。内容は `{window_s, collecting, dormant_since}` で、休止機能のないサービスでは `null` です。


### `FaceEntry`

ページ一覧の 1 項目。ページ ID `f`（1..=8）、テンプレート `t`、音声で付けた名前 `n`。

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `f` | uint8 | はい |  |  |
| `n` | string | はい |  |  |
| `t` | [`Template`](#template) | はい |  |  |

### `Focus`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid)? | いいえ |  |  |
| `seq` | uint16? | いいえ |  | デバイスからの最後の `channel.seq`。 |
| `source` | [`FocusSource`](#focussource)? | いいえ |  |  |

### `FocusReport`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid)? | いいえ |  |  |
| `source` | [`FocusSource`](#focussource) | はい |  |  |

### `FocusSource`

次のいずれか。`device`, `desktop`, `api`。


### `Group`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `count` | uint8 | はい |  |  |
| `index` | uint8 | はい |  |  |

### `Kind`

次のいずれか。`claude`, `codex`, `gemini`, `other`, `conol`, `grok`。


### `LastLinked`

最後の接続時のデバイスの状態を `state.json` に保存します。デバイスがなくても、サービスは再起動後も含め、最後に接続した時刻と動作していたバージョンを報告できます。

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `at` | [`Ts`](#ts) | はい |  | 接続中は通信可能になった時刻、切断後は接続が終了した時刻。 |
| `board` | string? | いいえ |  |  |
| `fw` | string? | いいえ |  |  |
| `transport` | [`Transport`](#transport)? | いいえ |  |  |

### `Link`

次のいずれか。`offline`, `connected`, `degraded`。


### `LinkDrop`

切断した時刻と、HID スレッドが報告した理由（`read`、`write`、`write_stall`、`removed`、`misses`）。

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `at` | [`Ts`](#ts) | はい |  |  |
| `why` | string | はい |  |  |

### `Listener`

デーモンが TCP ポートを確保できなかった場合に、そのポートを使用中のプロセス（§4.1 の `lsof -nP -iTCP:<port> -sTCP:LISTEN` で可能な範囲で調べます）。

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `command` | string? | いいえ |  |  |
| `pid` | uint64 | はい |  |  |

### `MacroKey`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `binding` | [`Binding`](#binding)? | いいえ |  | このキーを作動させる本体の操作部。`None` の場合はタッチ操作のみで、キー操作が割り当てられていればそれを送ります。 |
| `keys` | string? | いいえ |  | キーを押したときにデーモンが送るキー操作。例は `cmd+shift+4`（`keystroke_valid`）。`None` の場合はイベントのみです。 |
| `label` | string | はい |  |  |

### `MacroLayout`

ホストが保持するマクロパッドのキー。`device.macro.set` または本体のエディターで変更し、`state.json` に保存して再起動後に復元します。デバイスは変更を 1 件ずつ報告し、キー全体は報告しないため、これが唯一の完全なコピーです。

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `keys` | [`MacroKey`](#macrokey)[] | はい |  | スロット 0 から順に最大 9 個。空のラベルは空きスロットを示し、その後にあるキーの位置を保ちます。 |
| `src` | string? | いいえ |  | マクロパッドのヘッダーに表示する、最後に指定されたタグ。未指定の場合は `None` です。 |

### `MessageMeta`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `activity` | [`ActivityStatus`](#activitystatus)? | いいえ |  |  |
| `id` | string | はい |  |  |
| `images` | [`ChatImage`](#chatimage)[] | いいえ |  |  |
| `plan` | [`Plan`](#plan)? | いいえ |  | 明示的な公開プランの更新。通常のチャットやツールのメッセージでは省略します。 |
| `role` | [`ChatRole`](#chatrole) | はい |  |  |
| `runs` | array? | いいえ |  | 公開されるバックグラウンドワークフローのカウンター。親ターンとは独立しています。 |
| `streaming` | boolean | いいえ | `false` |  |
| `summary` | [`ActivitySummary`](#activitysummary)? | いいえ |  | ネイティブコンパニオンが提供する、公開用のアクティビティ名とプレビュー。 |

### `ModelOption`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `default_effort` | string | いいえ | `""` |  |
| `efforts` | string[] | いいえ | `[]` |  |
| `id` | string | はい |  |  |
| `label` | string | はい |  |  |

### `OpAnswer`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | はい |  |  |
| `card` | [`CardId`](#cardid) | はい |  |  |
| `choice` | uint8? | いいえ | `null` |  |
| `text` | string? | いいえ | `null` |  |

### `OpOrder`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | はい |  |  |
| `ctx` | string | はい |  |  |
| `source` | [`OrderSource`](#ordersource) | はい |  |  |
| `text` | string | はい |  |  |
| `utterance` | string? | いいえ | `null` | この指示の元になった PTT 発話の ID（B12）。§4.12 の `agent.order` には `utterance?` があり、[`crate::Order`] にも最初の週から同じフィールドがありました。しかし、指示を生成する唯一のリクエストに ID を入れる場所がなく、どちらにも値を渡せませんでした。そのためクライアントが指示を元の `transcript.final` と照合するにはテキストしか使えず、そのテキストも §5.6 の文法規則で書き換えられることがありました。キー入力（`source:"gui"`、`"api"`）では省略します。`default` があるため、既存の呼び出し元は送信する必要がありません。 |

### `OrderCap`

- string
- string — アダプター専用。


### `OrderOutcome`

デーモンからエージェントの所有者に渡された指示の配信結果。口述でもキー入力でも、`op.order` で送信した指示には `order` ID が付き、`agent.order` 通知および `agent.wait` の `{kind:"order"}` 結果として所有者に届きます。完了カードで選んだステップ（`ctx:"step"`）には ID がなく、デバイス自身が元に戻します。会話に指示を届けられなかった所有者は `ok: false` と `reason` を付けて 1 回だけ報告し、デバイスに未配信と表示されます。`ok: true` も受け付けますが、状態は変わりません。報告できるのは所有セッションだけです（他のセッションは `-32004`）。過去 10 分以内の未報告の指示だけが対象で（それ以外は `-32003`）、`reason` は所定の形式の 1 語に限ります（違反は `-32009`）。結果は `{rev, shown}` で、デバイスに渡した表示形式 `card`、`toast`、`none` のいずれかを返します。

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | はい |  | 指示の宛先のエージェント。 |
| `ok` | boolean | はい |  | 指示が会話に届いたかどうか。 |
| `order` | string | はい |  | 指示に付いていた `order` ID。 |
| `reason` | string? | いいえ | `null` | 届かなかった理由。小文字の英字、数字、アンダースコアからなる 1 語で、最大 32 バイトです（例：`backend_refused`）。省略時は `undeliverable` です。 |

### `OrderSource`

次のいずれか。`ptt`, `gui`, `api`。


### `Origin`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `group` | [`Group`](#group)? | いいえ |  |  |
| `kind` | [`OriginKind`](#originkind) | はい |  |  |
| `request_id` | string? | いいえ |  |  |
| `tool` | string? | いいえ |  |  |

### `OriginKind`

- string
- string — 組み込みアダプター専用。
- string — 任意のトークンまたはスクリプト。デバイスでは先頭に `» ` を付けます。


### `Owner`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `lease_expires` | [`Ts`](#ts)? | いいえ |  |  |
| `principal` | [`Principal`](#principal) | はい |  |  |
| `session` | [`SessionId`](#sessionid) | はい |  |  |

### `Page`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `count` | uint8 | はい |  |  |
| `index` | uint8 | はい |  |  |

### `Permission`

次のいずれか。`unknown`, `not_determined`, `granted`, `denied`。


### `Permissions`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `bluetooth` | [`Permission`](#permission) | いいえ | `"unknown"` | BLE 音声接続が最後に読み取った、このプロセスに対する CoreBluetooth の許可状態（`[voice] transport = "ble"`）。この経路で本体のマイクとスピーカーに必要なのは、マイク権限ではなく Bluetooth 権限です。CoreAudio 経由の場合、および接続を開始するまでは `unknown` です。 |
| `microphone` | [`Permission`](#permission) | はい |  |  |
| `speech` | [`Permission`](#permission) | はい |  |  |

### `PidKind`

次のいずれか。`cli`, `shell`, `unknown`。


### `Plan`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `explanation` | string? | いいえ |  |  |
| `steps` | [`PlanStep`](#planstep)[] | はい |  |  |

### `PlanStep`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `status` | [`StepStatus`](#stepstatus) | はい |  |  |
| `step` | string | はい |  |  |

### `Principal`

型は string。


### `PttCtx`

次のいずれか。`talk`, `answer`, `name`。


### `PttInfo`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid)? | いいえ |  |  |
| `card` | [`CardId`](#cardid)? | いいえ |  |  |
| `companion` | boolean | いいえ |  | この押下の音声はエージェントの所有者が文字起こしします（`caps.transcribe`）。ローカルの認識エンジンは予備に使うだけなので、その途中結果はデバイスに表示しません。 |
| `ctx` | [`PttCtx`](#pttctx) | はい |  |  |
| `live` | boolean | いいえ | `false` | 録音で **0 以外のサンプル** が得られたかどうか。§7.4 の `Arming`（false）と `Listening`（true）を区別します。この違いは操作する人にとって約 1 秒に相当します。このハードウェアの SCO 起動時間の中央値は 1031 ms（V1、2026-09-21 測定）で、その間もコールバックは届き、`AudioUnitRender` は `noErr` を返しますが、全サンプルが完全なデジタルゼロです。この時点で GUI に「聞き取り中」と表示すると実際の状態と異なります。コールバックだけでマイクの動作を判定すると、故障したマイクも検出できません（V2）。 |
| `principal` | [`Principal`](#principal) | はい |  | セッションを開始した主体。デバイスの `ptt` 信号による開始は `device`、それ以外は `voice.listen` の呼び出し元です（§7.4）。 |
| `session` | [`SessionId`](#sessionid)? | いいえ | `null` | 呼び出し元が使った **セッション**。`principal` だけでは特定できません（B2）。§4.9 では、ディスパッチ処理が生成するすべての `op.answer` / `op.order` に主体とセッションの両方が必要です。所有権は呼び出し主体単位ではなくセッション単位です（§4.2）。セッションがなければ、口述の回答を要求元の接続に対応付けられず、所有セッションの `agent.wait` にも結果を返せません。セッションを持たない本体の入力信号では `None` です。 |
| `since` | [`Ts`](#ts) | はい |  |  |
| `utterance` | string | いいえ | `""` | このセッションの各文字起こし結果に付く ID（`ut_<n>`、§4.12 の `transcript.partial\|final{utterance}` と B12 の `order.dispatched`）。他の処理で名前を付ける前に途中結果が届き始めるため、録音開始時に割り当てます。 |

### `Reply`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `announce` | string? | いいえ |  |  |
| `at` | [`Ts`](#ts) | はい |  |  |
| `redacted` | boolean | いいえ |  |  |
| `text` | string? | いいえ |  | コンテンツに分類されます。 |

### `ReplySet`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | はい |  |  |
| `announce` | string? | いいえ | `null` |  |
| `lane` | string? | いいえ | `null` |  |
| `message` | [`MessageMeta`](#messagemeta)? | いいえ | `null` | プロバイダーが付けた安定した項目 ID により、ストリーミング中も同じメッセージを更新できます。 |
| `speak` | boolean | いいえ | `false` |  |
| `text` | string | はい |  |  |

### `Route`

§7.2 の音声出力経路の状態遷移。B7 では §7.2 の **`ScoOurs`** に当たる値が `Sco` となっており、`Reopening` もありませんでした。このため、PTT の各押下後に約 2.5 秒続く状態（V1：A2DP 再開までの **中央値 2307 ms**）に名前がなく、`voice.route` でも報告できませんでした。`ScoOurs` が区別するのは、SCO に変わった原因です。自分のプロセスがマイクを開いた場合は PTT を離すとその状態から抜けます。他のアプリが開いた場合は終了を待つしかありません（V7：デバイスがシステム既定の入力なので、これは通常の動作です）。

- string
- string — 44.1 kHz で出力を開き、そのまま維持します（§7.2）。
- string — 自身の PTT 録音中。再生レーンを保留し、出力を閉じます。
- string — 他のアプリによる録音で出力レートが変わりました。
- string — 自身の録音終了後、A2DP に戻るまでの状態。V3 では 44100 に戻るまで待つ（2591 ms）代わりに、デバイスの **現在の** 公称レートで直ちに出力を開きます（最初のオーディオレンダーコールバックまで 48 ms）。PTT 後の通知ごとに 54 倍の差が出ます。約 2.3 秒の待ち時間は macOS の SCO 終了タイマーによるもので、短縮できません。
- string — 出力を 2 回開き直しても失敗しました（§7.2）。異なるレートで開いた出力に合わせてリサンプリングし、`voice.route{stuck:true}` を通知します。次のレート変更で復帰します。
- string — `[voice] fallback_output = "system_default"`。デバイスではなくシステム既定の出力を使います。
- string — BLE 音声接続（`[voice] transport = "ble"`）。デバイスのスピーカーの GATT キャラクタリスティックを 16 kHz で開いたままにします。Classic デバイスの `A2dp` に相当する、この接続方式の定常状態です。
- string — BLE 音声接続で自身の PTT 録音中。押している間は本体スピーカーを閉じ（半二重、エコーキャンセルなし、V5）、離すと開き直します。BLE 接続での `ScoOurs` に相当します。


### `Run`

プロバイダーが報告したバックグラウンド実行。件数はワーカー数であり、プランのステップ数を推定したものではありません。

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `completed` | uint32 | はい |  |  |
| `elapsed_ms` | uint64 | はい |  |  |
| `id` | string | はい |  |  |
| `name` | string | はい |  |  |
| `status` | [`ActivityStatus`](#activitystatus) | はい |  |  |
| `tokens` | uint64 | はい |  |  |
| `total` | uint32 | はい |  |  |

### `ServiceState`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `degraded` | string? | いいえ |  |  |
| `dormancy` | [`DormancyState`](#dormancystate)? | いいえ | `null` | 収集処理の休止状態。休止機能のないサービス（`--no-device` で起動したもの、または休止機能追加前のもの）では `null` です。 |
| `listener` | [`Listener`](#listener)? | いいえ | `null` | `degraded` が `port_in_use` または `hooks_hijack_risk` の間に設定します。`service.degraded` と `service.info` に、ポートを使っているプロセスを示すための情報です（§4.1、§4.12）。 |
| `port` | uint16 | はい |  |  |
| `version` | string | はい |  |  |

### `SessionControls`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `editable` | boolean | いいえ | `false` |  |
| `effort` | string | はい |  |  |
| `model` | string | はい |  |  |
| `models` | [`ModelOption`](#modeloption)[] | いいえ | `[]` |  |
| `note` | string | いいえ | `""` |  |

### `SessionId`

型は string。


### `SessionInfo`

エージェントの実行場所。`key` 以外はコンテンツとして扱います。

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `cwd` | string? | いいえ |  |  |
| `key` | string? | いいえ |  | `"claude:<session_id>"`、`"myapp:<id>"` はべき等性キーの例です。 |
| `pid` | uint32? | いいえ |  |  |
| `pid_kind` | [`PidKind`](#pidkind)? | いいえ |  |  |
| `terminal` | [`Terminal`](#terminal)? | いいえ |  |  |

### `SessionSet`

所有者の認証を確認した設定報告。通常の再取得では `request_rev` を省略します。

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | はい |  |  |
| `controls` | [`SessionControls`](#sessioncontrols) | はい |  |  |
| `request_rev` | uint16? | いいえ | `null` | 適用してから、対象の `session.settings_requested` のリビジョンを正確に指定して確認応答します。変更に失敗したときは、実際の設定と空でない説明を報告してください。 |

### `SessionState`

§6.3.

次のいずれか。`absent`, `linked`, `await_hello`, `syncing`, `live`, `degraded`。


### `SetKey`

- string
- string — API の設定。デバイスへはバージョン付きの `language` メッセージで送ります。
- string — 受信した文書をデバイスに表示するサイズ。0 は小、1 は中、2 は大。テキストの折り返しを決めるのは **ホスト** なので、ホストもこの値を知る必要があります。双方でサイズが違うと各行の折り返し位置がずれます。


### `Settings`

ホストに保存し、`boot:1` で復元する `set` のキー。

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `announce` | uint16 | いいえ | `1` |  |
| `arcadesound` | uint16 | いいえ | `0` | 0 はオフ、1 は効果音のみ、2 は音楽と効果音。「ゲーム」のメニューで変更します。 |
| `autojump` | uint16 | いいえ | `0` |  |
| `brightness` | uint16 | いいえ | `5` |  |
| `chime` | uint16 | いいえ | `1` |  |
| `demo` | uint16 | いいえ | `0` |  |
| `idlehome` | uint16 | いいえ | `120` |  |
| `language` | uint16 | いいえ | `0` | 対応デバイスと共有する画面の言語。en、zh-Hans、zh-Hant、ja。 |
| `language_revision` | uint32 | いいえ | `0` |  |
| `mic` | uint16 | いいえ | `6` |  |
| `speaker` | uint16 | いいえ | `6` |  |
| `textsize` | uint16 | いいえ | `1` | 0 は小、1 は中、2 は大。**ホスト** がここで選ばれた書体に合わせて文書を折り返すため、デバイスだけの設定ではありません。 |

### `Slot`

デバイスのスロット番号 1..=255（通信上は 1 バイト）。デバイスが保持できる数は `DeviceState::capacity` で、容量を報告しない機種では 8 個です。

型は uint8。


### `Source`

次のいずれか。`hook`, `api`, `mcp`。


### `Speaking`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid)? | いいえ |  |  |
| `lane` | string | はい |  |  |
| `utterance` | string | はい |  |  |

### `StepStatus`

次のいずれか。`pending`, `in_progress`, `completed`。


### `SttInfo`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `engine` | string | はい |  |  |
| `locale` | string | はい |  |  |
| `on_device` | boolean | はい |  |  |

### `Template`

次のいずれか。`agents`, `omarchy`, `macropad`, `usage`, `settings`, `arcade`, `system`, `timer`。


### `Terminal`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `app` | string? | いいえ |  | セッションが動作するアプリの macOS バンドル識別子（`dev.zed.Zed`、`com.mitchellh.ghostty`、`com.anthropic.claudefordesktop`）。フックヘルパーが特定します。セッションの `Open app` カードを押すと該当アプリが前面に出ます。接続アプリが登録したエージェントでは参考情報のみです。デーモンが前面に出すのは、フック経由で登録されたセッションのアプリに限ります。 |
| `inbox` | string? | いいえ |  | セッション専用のメッセージ受信箱。Claude Code がセッション間通信のためにバインドする Unix ソケット（`CLAUDE_CODE_MESSAGING_SOCKET`）で、フックヘルパーの環境から取得します。受信箱のあるエージェントには、ターミナルへのキー入力の代わりにこのソケットに指示を送ります（§5.5）。フォーカスの切り替え、AppleScript、TCC の許可は不要です。併用するセッションごとのトークンは **含まれません**。デーモン内部（`adapters::rt::Adapters`）にのみ保持し、スナップショットには含めません。 |
| `iterm_session` | string? | いいえ |  |  |
| `program` | string? | いいえ |  |  |
| `term_session` | string? | いいえ |  |  |
| `title_stamp` | string? | いいえ |  | Ghostty のタブ経由の処理を削除したため、現在は書き込みません。このフィールドを送る既存クライアントのリクエストを解析できるように残しています。 |
| `tmux_pane` | string? | いいえ |  |  |
| `tty` | string? | いいえ |  |  |

### `TitleSource`

- string — `session_name` / `name`。状態情報として扱います。
- string — 作業ディレクトリ（cwd）に由来し、状態情報として扱います。
- string — 操作者のプロンプトの先頭 64 B。コンテンツとして扱います。
- string — ツール入力の先頭部分。コンテンツとして扱います。
- string — `agent.update` で設定します。状態情報として扱います。


### `Tone`

次のいずれか。`blue`, `amber`, `green`, `red`, `gray`。


### `TranscribeCap`

PTT で録音した音声を文字起こしできる主体（`caps.transcribe`）。

- string — エージェント宛ての発話ごとに所有者が `agent.transcribe` を受け取り、`agent.transcribe.result` で応答します。
- string — デーモン自身の認識エンジンのみ。


### `Transport`

このパソコンとデバイスの接続方式。HID スレッドがデバイスを開いたバスを示します。

次のいずれか。`usb`, `bluetooth`。


### `Ts`

型は string。


### `TtsInfo`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `engine` | string | はい |  |  |

### `Usage`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `at` | [`Ts`](#ts)? | いいえ |  |  |
| `items` | [`UsageItem`](#usageitem)[] | はい |  |  |
| `period` | string | はい |  |  |

### `UsageItem`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `account` | string? | いいえ |  | プロバイダーが特定できる場合は、この利用枠が属するアカウント。 |
| `name` | string | はい |  |  |
| `percent` | uint8 | はい |  | 0..=100. |
| `reset` | string | はい |  |  |
| `reset_tip` | string? | いいえ |  | 有効期限が近い、引き換え可能なリセットクレジットの通知。該当する期限がなければ省略します。 |
| `tone` | [`Tone`](#tone)? | いいえ |  | `None` の場合、バーの色を `percent` から決めます。 |

### `VoiceInput`

PTT セッションが開くマイク（§4.9 の `voice.listen{input}`）。

- string — デバイス自身の入力 UID。デバイスの `ptt` 入力信号が指すのは常にこのマイクです。
- string — Mac の既定の入力デバイス。これを要求できるのは `voice.listen{input:"mac"}` だけで、PTT を押してもこのマイクに切り替わることはありません。


### `VoiceListen`

`voice.listen`（§4.9）。デバイスの `ptt` 入力信号で動く §7.4 と同じ状態機械を GUI/API から操作する入り口です。そこから派生する処理には常に **呼び出し元** の主体とセッションを付け、`device` は使いません（B2）。

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | はい |  |  |
| `card` | [`CardId`](#cardid)? | いいえ | `null` |  |
| `ctx` | string | いいえ | `"talk"` | `"talk"` または `"answer"`。§4.9 で認めるのはこの 2 種類だけなので、[`crate::PttCtx`] ではなく文字列を使います。`name` はデバイスからの入力信号であり、API の呼び出し元が生成することはできません。 |
| `input` | [`VoiceInput`](#voiceinput) | いいえ | `"deck"` | §4.9 の `input`。`"deck"`（既定値）または `"mac"`。 |
| `max_s` | uint32 | いいえ | `60` |  |

### `VoiceSay`

`voice.say` (§4.9).

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid)? | いいえ | `null` |  |
| `interrupt` | boolean | いいえ | `false` | §4.9。優先度が高い再生レーンには割り込めません。ミキサーが判断します。 |
| `lane` | string | はい |  |  |
| `text` | string | はい |  |  |
| `ttl_s` | uint32? | いいえ | `null` | §4.9 の `ttl_s?:30`。期限までに再生が始まらなければ `chatter` を破棄します。ここで期限を捨てずにミキサーに渡します。呼び出し元が指定した期限を通知なく無視するより、要求自体を拒否するほうがよいためです。 |
| `voice` | string? | いいえ | `null` |  |

### `VoiceState`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `input_uid` | string? | いいえ |  |  |
| `output_uid` | string? | いいえ |  |  |
| `permissions` | [`Permissions`](#permissions) | はい |  |  |
| `ptt` | [`PttInfo`](#pttinfo)? | いいえ |  |  |
| `route` | [`Route`](#route) | はい |  |  |
| `speaking` | [`Speaking`](#speaking)? | いいえ |  |  |
| `stt` | [`SttInfo`](#sttinfo) | はい |  |  |
| `transport` | [`VoiceTransport`](#voicetransport)? | いいえ | `null` | 音声エンジンが対象とするトランスポート（`[voice] transport`）。エンジンが報告するまで、およびエンジンを含まないビルドでは null です。 |
| `tts` | [`TtsInfo`](#ttsinfo) | はい |  |  |

### `VoiceTransport`

デバイスの音声経路（`[voice] transport`）。BLE 接続上の本体の GATT 音声サービス（`deckd::ble`）、または Rev A より前の Bluetooth Classic 機で CoreAudio 経由で使う HFP/A2DP の入力・出力デバイスの組です。

- string
- string — Rev A ファームウェアは Bluetooth Classic 音声デバイスとしては動作しないため、これが既定値です。


### `Window`

| フィールド | 型 | 必須 | 既定値 | 説明 |
|---|---|---|---|---|
| `title` | string | はい |  |  |
| `tone` | [`Tone`](#tone)? | いいえ |  | 省略時は灰色で表示します。 |

この日本語版は機械翻訳です。日本語を母語とする方による逐語確認は未実施です。仕様の解釈には英語原文を参照してください。
