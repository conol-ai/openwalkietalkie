# `deckd` — the v1 API

<!-- GENERATED FILE — DO NOT EDIT. -->

This file is generated from what the running daemon serves on `GET /v1/schema`
(the JSON Schema of every method, event and type, from the one Rust source in
`deck-model`, plus the dispatch table). Editing it by hand is pointless: the
`deckd` test `the_checked_in_api_document_is_what_the_schema_produces` compares
the file with the generator's output on every run. To change it, change the
Rust types or the method table and regenerate:

```sh
cargo run --locked -p deckd --target aarch64-apple-darwin --target-dir target/host --manifest-path host/Cargo.toml -- api-doc > docs/deckd-api.md
```

The prose that explains *why* the API is shaped this way — transports, principals
and scopes, content classes, the delivery contract, the error codes — is
[`docs/host-service.md` §4](host-service.md#4-the-open-api). Nothing is written
here that the schema does not carry, because anything written here could go
stale and nothing would notice.

## Methods

`scope` is the least scope a principal must hold (§4.2); a method with no scope
is answerable before the handshake. `answered by` is `store` for a method the
store's reducer answers from the snapshot and `core` for one the core thread
answers out of band. `params` links the type the request carries.

| method | scope | answered by | params |
|---|---|---|---|
| `session.hello` | — (no scope) | core | — (not in the `Request` enum) |
| `session.subscribe` | — (no scope) | core | — (not in the `Request` enum) |
| `session.unsubscribe` | — (no scope) | core | — (not in the `Request` enum) |
| `session.ping` | — (no scope) | core | — (not in the `Request` enum) |
| `state.get` | `observe` | core | — (not in the `Request` enum) |
| `state.events` | `observe` | core | — (not in the `Request` enum) |
| `agent.list` | `observe` | core | — (not in the `Request` enum) |
| `agent.get` | `observe` | core | — (not in the `Request` enum) |
| `image.get` | `observe:content` | core | — (not in the `Request` enum) |
| `device.get` | `observe` | core | — (not in the `Request` enum) |
| `voice.get` | `observe` | core | — (not in the `Request` enum) |
| `service.info` | `observe` | core | — (not in the `Request` enum) |
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
| `agent.wait` | `agent` | core | — (not in the `Request` enum) |
| `op.answer` | `operate` | store | [`OpAnswer`](#opanswer) |
| `op.order` | `operate` | store | [`OpOrder`](#oporder) |
| `op.focus` | `operate` | store | {agent: [`AgentId`](#agentid)} |
| `op.snooze` | `operate` | store | {agent: [`AgentId`](#agentid), card: [`CardId`](#cardid)} |
| `op.ack` | `operate` | store | {agent: [`AgentId`](#agentid), card: [`CardId`](#cardid)} |
| `voice.say` | `voice` | store | [`VoiceSay`](#voicesay) |
| `voice.stop` | `voice` | store | {all?: boolean, lane?: string?, utterance?: string?} |
| `voice.listen` | `voice` | store | [`VoiceListen`](#voicelisten) |
| `voice.cancel` | `voice` | store | {} |
| `voice.engines` | `voice` | core | — (not in the `Request` enum) |
| `voice.test` | `voice` | core | — (not in the `Request` enum) |
| `firmware.status` | `observe` | core | — (not in the `Request` enum) |
| `firmware.check` | `admin` | core | — (not in the `Request` enum) |
| `firmware.install` | `admin` | core | — (not in the `Request` enum) |
| `stt.status` | `voice` | core | — (not in the `Request` enum) |
| `stt.install` | `admin` | core | — (not in the `Request` enum) |
| `stt.remove` | `admin` | core | — (not in the `Request` enum) |
| `tts.status` | `voice` | core | — (not in the `Request` enum) |
| `tts.install` | `admin` | core | — (not in the `Request` enum) |
| `tts.remove` | `admin` | core | — (not in the `Request` enum) |
| `perm.status` | `voice` | core | — (not in the `Request` enum) |
| `perm.request` | `admin` | core | — (not in the `Request` enum) |
| `device.set` | `device` | store | [`DeviceSet`](#deviceset) |
| `device.faces.set` | `device` | store | {list: [`FaceEntry`](#faceentry)[]} |
| `device.usage.set` | `device` | store | {items: [`UsageItem`](#usageitem)[], period: string} |
| `device.macro.set` | `device` | store | {f: uint8, keys: [`MacroKey`](#macrokey)[], layout: string, src?: string?} |
| `device.ws.set` | `device` | store | {active: uint8, count: uint8, sel: uint8, windows: [`Window`](#window)[]} |
| `device.name` | `device` | store | {text: string} |
| `focus.report` | `device` | store | [`FocusReport`](#focusreport) |
| `config.get` | `admin` | core | — (not in the `Request` enum) |
| `config.set` | `admin` | core | — (not in the `Request` enum) |
| `config.reload` | `admin` | core | — (not in the `Request` enum) |
| `token.create` | `admin` | core | — (not in the `Request` enum) |
| `token.list` | `admin` | core | — (not in the `Request` enum) |
| `token.revoke` | `admin` | core | — (not in the `Request` enum) |
| `token.rotate` | `admin` | core | — (not in the `Request` enum) |
| `metrics.get` | `admin` | core | — (not in the `Request` enum) |
| `integrations.status` | `admin` | core | — (not in the `Request` enum) |
| `integrations.install` | `admin` | core | — (not in the `Request` enum) |
| `integrations.uninstall` | `admin` | core | — (not in the `Request` enum) |
| `integrations.repair` | `admin` | core | — (not in the `Request` enum) |
| `log.tail` | `admin` | core | — (not in the `Request` enum) |
| `device.flood` | `admin` | core | — (not in the `Request` enum) |
| `device.sync` | `admin` | core | — (not in the `Request` enum) |
| `service.restart` | `admin` | core | — (not in the `Request` enum) |
| `service.stop` | `admin` | core | — (not in the `Request` enum) |
| `hooks.claude` | `hooks:ingest` | core | — (not in the `Request` enum) |
| `hooks.codex` | `hooks:ingest` | core | — (not in the `Request` enum) |
| `hooks.gemini` | `hooks:ingest` | core | — (not in the `Request` enum) |
| `hooks.codex_notify` | `hooks:ingest` | core | — (not in the `Request` enum) |
| `hooks.claude_statusline` | `hooks:ingest` | core | — (not in the `Request` enum) |

## Events

Server push is a JSON-RPC notification `{"method":"event", "params": Event}`
(§4.12). `data` is redacted per subscriber at fan-out, so what a given session
sees depends on its scopes.

| field | type | required | default | description |
|---|---|---|---|---|
| `data` | any | yes |  |  |
| `principal` | [`Principal`](#principal) | yes |  |  |
| `rev` | uint64 | yes |  |  |
| `ts` | [`Ts`](#ts) | yes |  |  |
| `type` | [`EventKind`](#eventkind) | yes |  |  |

`type` is one of:


## Snapshot

| field | type | required | default | description |
|---|---|---|---|---|
| `agents` | object | yes |  |  |
| `device` | [`DeviceState`](#devicestate) | yes |  |  |
| `faces` | [`FaceEntry`](#faceentry)[] | yes |  | The last adopted strip (restored on `boot:1`). |
| `focus` | [`Focus`](#focus) | yes |  |  |
| `rev` | uint64 | yes |  | Increases only when something observable changed. |
| `service` | [`ServiceState`](#servicestate) | yes |  |  |
| `settings` | [`Settings`](#settings) | yes |  | Host-persisted settings (restored on `boot:1`). |
| `usage` | [`Usage`](#usage)? | no |  |  |
| `voice` | [`VoiceState`](#voicestate) | yes |  |  |

## Errors

A JSON-RPC error of §4.4 (`code`, `message`, `data`).

| field | type | required | default | description |
|---|---|---|---|---|
| `code` | int32 | yes |  |  |
| `data` | any | yes |  |  |
| `message` | string | yes |  |  |

## Link events

From the hid thread (§6.2, §6.3): what happened to the link.

- {link: string} — The device was opened; `hello` is on its way.
- {link: string} — Read/write error, removal, or 6 hello misses: handle dropped.
- {link: string} — 3 hello misses; handle kept.
- {link: string, sid: uint32} — A hello reply with our sid: the session is live.
- {boot: boolean, link: string, sid: uint32} — A hello reply with a foreign sid: a barrier is being written.
- {link: string, sid: uint32} — The barrier's `synced` was written.
- {link: string} — The core→hid channel overflowed; the core re-projects.
- {link: string, pace_ms: uint16} — Two drops after bursts: pace raised (ms).


## Types

The 84 named types the schemas above refer to.

### `ActivityStatus`

One of: `running`, `completed`, `failed`, `stopped`.


### `Agent`

| field | type | required | default | description |
|---|---|---|---|---|
| `caps` | [`Caps`](#caps) | yes |  |  |
| `card` | [`Card`](#card)? | no |  |  |
| `chat_rev` | uint16 | no | `0` | Stable while messages grow; changes when retained history is pruned. |
| `companion` | [`CompanionProfile`](#companionprofile)? | no |  | Optional native companion grouping and launcher/session identity. |
| `id` | [`AgentId`](#agentid) | yes |  |  |
| `kind` | [`Kind`](#kind) | yes |  |  |
| `messages` | [`ChatMessage`](#chatmessage)[] | no | `[]` | Recent conversation, bounded to 64 messages / 512 KiB per agent. |
| `meta` | any | yes |  | Content. |
| `name` | string | yes |  |  |
| `owner` | [`Owner`](#owner) | yes |  |  |
| `pinned` | boolean | yes |  |  |
| `redacted` | boolean | no |  |  |
| `reply` | [`Reply`](#reply)? | no |  |  |
| `reply_rev` | uint16 | no | `0` | Bumped whenever `reply` changes. The `reply` line carries it and so does the window the host lays out, so the deck can tell a window of the text it is showing from one of the text before it. |
| `session` | [`SessionInfo`](#sessioninfo) | yes |  |  |
| `since` | [`Ts`](#ts) | yes |  | When `state` was entered. |
| `slot` | [`Slot`](#slot)? | no |  | The device slot, a projection; `None` = "not on deck". |
| `source` | [`Source`](#source) | yes |  |  |
| `state` | [`AgentState`](#agentstate) | yes |  |  |
| `title` | string? | no |  | Content when `title_source` is `prompt`/`tool`. |
| `title_source` | [`TitleSource`](#titlesource) | yes |  |  |
| `voice` | string? | no |  |  |
| `workflow` | [`Plan`](#plan)? | no |  |  |
| `workflow_runs` | [`Run`](#run)[] | no |  |  |

### `AgentId`

`"ag_…"`: stable for the life of the agent.

Type: string.


### `AgentRegister`

| field | type | required | default | description |
|---|---|---|---|---|
| `caps` | [`Caps`](#caps) | yes |  |  |
| `cwd` | string? | no | `null` |  |
| `key` | string? | no | `null` |  |
| `kind` | [`Kind`](#kind) | yes |  |  |
| `lease_ttl_s` | uint32 | no | `0` | `lease_ttl_s:30` in §4.6 is the daemon's default, not the caller's duty: 0 (the absent value) takes `[agents] lease_ttl_s`. |
| `meta` | any | no | `null` |  |
| `name` | string | yes |  |  |
| `pid` | uint32? | no | `null` |  |
| `pid_kind` | [`PidKind`](#pidkind)? | no | `null` | What `pid` is (§5): the hook helper's parent walk resolves `cli` for the CLI process itself, `shell` for the `sh -c` in between, and `unknown` when no CLI ancestor was found. `kill(pid, 0)` liveness polling runs **only** for `cli`, so an unset value is never polled. |
| `slot_hint` | [`Slot`](#slot)? | no | `null` |  |
| `terminal` | [`Terminal`](#terminal)? | no | `null` |  |
| `voice` | string? | no | `null` |  |

### `AgentState`

One of: `working`, `question`, `ready`, `error`, `idle`.


### `AgentUpdate`

| field | type | required | default | description |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | yes |  |  |
| `meta` | any | no |  |  |
| `name` | string? | no |  |  |
| `since` | [`Ts`](#ts)? | no |  |  |
| `state` | [`AgentState`](#agentstate)? | no |  |  |
| `title` | string? | no |  |  |
| `title_source` | [`TitleSource`](#titlesource)? | no |  |  |

### `AnswerCap`

- string
- string — Adapter-only.


### `Binding`

One of: `left`, `right`, `enter`, `cw`, `ccw`, `shift+left`, `shift+right`, `shift+enter`.


### `Caps`

| field | type | required | default | description |
|---|---|---|---|---|
| `answer` | [`AnswerCap`](#answercap) | yes |  |  |
| `focus` | boolean | no | `false` | §4.15 registers with `caps:{answer,order}` alone: an agent that does not say it can be focused cannot be. |
| `order` | [`OrderCap`](#ordercap) | yes |  |  |

### `Card`

The card status machine of §4.8.

- {status: string}
- {choice?: uint8?, status: string, text?: string?}
- {status: string}
- {status: string}
- {reason: string, status: string}
- {status: string}
- {focused_at?: [`Ts`](#ts)?, status: string}


### `CardClear`

| field | type | required | default | description |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | yes |  |  |
| `card` | [`CardId`](#cardid) | yes |  |  |
| `delivered` | boolean | yes |  |  |
| `reason` | string? | no | `null` |  |

### `CardId`

Per-slot card id, `last + 1` with wrap-around skipping 0 (§6.5); the default 0 is "no card yet" (never on the wire).

Type: uint16.


### `CardKind`

One of: `q`, `r`.


### `CardSet`

`agent.card.set`.

| field | type | required | default | description |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | yes |  |  |
| `choices` | [`Choice`](#choice)[] | no | `[]` |  |
| `deliver` | [`Deliver`](#deliver) | yes |  |  |
| `free_text` | boolean | no | `true` |  |
| `id` | [`CardId`](#cardid)? | no |  | Re-push under an existing id (same-id content update, no new chime). |
| `origin` | [`Origin`](#origin) | no | `{"group":null,"kind":"custom","request_id":null,"tool":null}` | §4.6: a card from any token or script is `custom` (prefixed `» ` on the deck) unless it names its own origin. |
| `rec` | uint8? | no | `null` |  |
| `text` | string | yes |  |  |
| `timeout_s` | uint32? | no | `null` |  |
| `type` | [`CardKind`](#cardkind) | yes |  |  |

### `ChatImage`

Small references only. Image bytes never enter snapshots or the event ring.

| field | type | required | default | description |
|---|---|---|---|---|
| `height` | uint32 | yes |  |  |
| `id` | string | yes |  |  |
| `label` | string | yes |  |  |
| `width` | uint32 | yes |  |  |

### `ChatMessage`

| field | type | required | default | description |
|---|---|---|---|---|
| `activity` | [`ActivityStatus`](#activitystatus)? | no |  |  |
| `at` | [`Ts`](#ts) | yes |  |  |
| `id` | string | yes |  |  |
| `images` | [`ChatImage`](#chatimage)[] | no |  |  |
| `plan` | [`Plan`](#plan)? | no |  | Explicit public plan updates; absent on ordinary chat/tool messages. |
| `role` | [`ChatRole`](#chatrole) | yes |  |  |
| `runs` | array? | no |  | Public background workflow counters, independent of the parent turn. |
| `streaming` | boolean | no | `false` |  |
| `text` | string | yes |  |  |

### `ChatRole`

One of: `user`, `assistant`, `tool`.


### `Choice`

| field | type | required | default | description |
|---|---|---|---|---|
| `detail` | string? | no |  |  |
| `label` | string | yes |  |  |

### `CompanionProfile`

A client-owned group. Identifiers partition slot promotion; labels are public.

| field | type | required | default | description |
|---|---|---|---|---|
| `app_id` | string | yes |  | Stable application identifier, 1–64 ASCII letters, digits, dots, dashes or underscores. |
| `group_id` | string | yes |  | Stable account/workspace identifier within this app, 1–128 bytes. Do not put secrets here. |
| `group_label` | string | yes |  | Roster heading, 1–24 UTF-8 bytes; use distinct headings for distinct device pages. |
| `role` | [`CompanionRole`](#companionrole) | yes |  |  |

### `CompanionRole`

One of: `session`, `launcher`.


### `CompanionSet`

| field | type | required | default | description |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | yes |  |  |
| `profile` | [`CompanionProfile`](#companionprofile)? | no |  | Null removes native presentation. Existing registrations remain valid. |

### `Deliver`

- string
- string — Adapter-only: a parked hook response.
- string — ENTER focuses the terminal; degraded holds and informational cards.


### `DeviceSet`

§4.10 names the two fields `k` and `v`, like the `set` line on the wire and the `device.set` event.

| field | type | required | default | description |
|---|---|---|---|---|
| `k` | [`SetKey`](#setkey) | yes |  |  |
| `v` | uint16 | yes |  |  |

### `DeviceState`

| field | type | required | default | description |
|---|---|---|---|---|
| `boot` | boolean? | no |  |  |
| `err_counts` | [`ErrCounts`](#errcounts) | yes |  |  |
| `face` | uint8? | no |  | Unknown until the first `face` event. |
| `faces` | [`FaceEntry`](#faceentry)[] | yes |  | The strip as the device reports it. |
| `fw` | string? | no |  |  |
| `host_online` | boolean | yes |  |  |
| `link` | [`Link`](#link) | yes |  |  |
| `outbox_depth` | uint32 | yes |  |  |
| `pace_ms` | uint16 | yes |  |  |
| `session` | [`SessionState`](#sessionstate) | yes |  |  |
| `settings` | [`Settings`](#settings) | yes |  | The device's settings as last reported/pushed. |
| `sid` | uint32 | yes |  | This daemon's session id (0 until `Store::new` is told one). |

### `ErrCounts`

| field | type | required | default | description |
|---|---|---|---|---|
| `parse` | uint32 | yes |  |  |
| `range` | uint32 | yes |  |  |
| `size` | uint32 | yes |  |  |
| `unknown` | uint32 | yes |  |  |

### `EventKind`

Event types of §4.12, serialized as their dotted names.

- string
- string — The deck's own key editor changed a slot (`device.macro.edit`).


### `FaceEntry`

One strip entry: face id `f` (1..=8), template `t`, spoken name `n`.

| field | type | required | default | description |
|---|---|---|---|---|
| `f` | uint8 | yes |  |  |
| `n` | string | yes |  |  |
| `t` | [`Template`](#template) | yes |  |  |

### `Focus`

| field | type | required | default | description |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid)? | no |  |  |
| `seq` | uint16? | no |  | The last device `channel.seq`. |
| `source` | [`FocusSource`](#focussource)? | no |  |  |

### `FocusReport`

| field | type | required | default | description |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid)? | no |  |  |
| `source` | [`FocusSource`](#focussource) | yes |  |  |

### `FocusSource`

One of: `device`, `desktop`, `api`.


### `Group`

| field | type | required | default | description |
|---|---|---|---|---|
| `count` | uint8 | yes |  |  |
| `index` | uint8 | yes |  |  |

### `Kind`

One of: `claude`, `codex`, `gemini`, `other`, `conol`.


### `Link`

One of: `offline`, `connected`, `degraded`.


### `Listener`

Who holds the TCP port when the daemon could not take it (§4.1's `lsof -nP -iTCP:<port> -sTCP:LISTEN` lookup, best effort).

| field | type | required | default | description |
|---|---|---|---|---|
| `command` | string? | no |  |  |
| `pid` | uint64 | yes |  |  |

### `MacroKey`

| field | type | required | default | description |
|---|---|---|---|---|
| `binding` | [`Binding`](#binding)? | no |  | A hardware control of the deck that presses this key; `None` = touch (and the keystroke, if any) only. |
| `keys` | string? | no |  | The keystroke the daemon posts when the key is pressed: `cmd+shift+4` (`keystroke_valid`). `None` = an event only. |
| `label` | string | yes |  |  |

### `MessageMeta`

| field | type | required | default | description |
|---|---|---|---|---|
| `activity` | [`ActivityStatus`](#activitystatus)? | no |  |  |
| `id` | string | yes |  |  |
| `images` | [`ChatImage`](#chatimage)[] | no |  |  |
| `plan` | [`Plan`](#plan)? | no |  | Explicit public plan updates; absent on ordinary chat/tool messages. |
| `role` | [`ChatRole`](#chatrole) | yes |  |  |
| `runs` | array? | no |  | Public background workflow counters, independent of the parent turn. |
| `streaming` | boolean | no | `false` |  |

### `ModelOption`

| field | type | required | default | description |
|---|---|---|---|---|
| `default_effort` | string | no | `""` |  |
| `efforts` | string[] | no | `[]` |  |
| `id` | string | yes |  |  |
| `label` | string | yes |  |  |

### `OpAnswer`

| field | type | required | default | description |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | yes |  |  |
| `card` | [`CardId`](#cardid) | yes |  |  |
| `choice` | uint8? | no | `null` |  |
| `text` | string? | no | `null` |  |

### `OpOrder`

| field | type | required | default | description |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | yes |  |  |
| `ctx` | string | yes |  |  |
| `source` | [`OrderSource`](#ordersource) | yes |  |  |
| `text` | string | yes |  |  |
| `utterance` | string? | no | `null` | The PTT utterance this order was dictated from (B12).  §4.12 gives `agent.order` an `utterance?` and [`crate::Order`] has carried the field since week 1, but nothing could ever fill either: the one request that produces an order had nowhere to put the id, so a client that wanted to line an order up against the `transcript.final` it came from had only the text to match on — and the text is exactly what the §5.6 grammar may have rewritten.  Absent for anything typed (`source:"gui"`, `"api"`); `default` so no existing caller has to send it. |

### `OrderCap`

- string
- string — Adapter-only.


### `OrderSource`

One of: `ptt`, `gui`, `api`.


### `Origin`

| field | type | required | default | description |
|---|---|---|---|---|
| `group` | [`Group`](#group)? | no |  |  |
| `kind` | [`OriginKind`](#originkind) | yes |  |  |
| `request_id` | string? | no |  |  |
| `tool` | string? | no |  |  |

### `OriginKind`

- string
- string — Built-in adapters only.
- string — Any token or script; prefixed `» ` on the deck.


### `Owner`

| field | type | required | default | description |
|---|---|---|---|---|
| `lease_expires` | [`Ts`](#ts)? | no |  |  |
| `principal` | [`Principal`](#principal) | yes |  |  |
| `session` | [`SessionId`](#sessionid) | yes |  |  |

### `Page`

| field | type | required | default | description |
|---|---|---|---|---|
| `count` | uint8 | yes |  |  |
| `index` | uint8 | yes |  |  |

### `Permission`

One of: `unknown`, `not_determined`, `granted`, `denied`.


### `Permissions`

| field | type | required | default | description |
|---|---|---|---|---|
| `bluetooth` | [`Permission`](#permission) | no | `"unknown"` | CoreBluetooth's authorization for this process, as the BLE voice link last read it (`[voice] transport = "ble"`): what the deck's microphone and speaker need there, instead of Microphone. `unknown` on the CoreAudio transport and until the link has started. |
| `microphone` | [`Permission`](#permission) | yes |  |  |
| `speech` | [`Permission`](#permission) | yes |  |  |

### `PidKind`

One of: `cli`, `shell`, `unknown`.


### `Plan`

| field | type | required | default | description |
|---|---|---|---|---|
| `explanation` | string? | no |  |  |
| `steps` | [`PlanStep`](#planstep)[] | yes |  |  |

### `PlanStep`

| field | type | required | default | description |
|---|---|---|---|---|
| `status` | [`StepStatus`](#stepstatus) | yes |  |  |
| `step` | string | yes |  |  |

### `Principal`

Type: string.


### `PttCtx`

One of: `talk`, `answer`, `name`.


### `PttInfo`

| field | type | required | default | description |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid)? | no |  |  |
| `card` | [`CardId`](#cardid)? | no |  |  |
| `ctx` | [`PttCtx`](#pttctx) | yes |  |  |
| `live` | boolean | no | `false` | Whether the capture has produced a **non-zero sample** yet — §7.4's `Arming` (false) vs `Listening` (true).  The distinction is a second of the operator's life, not a detail: SCO bring-up on this hardware is 1031 ms median (V1, measured 2026-09-21), during which callbacks arrive, `AudioUnitRender` returns `noErr` and every sample is an exact digital zero. A GUI that shows "listening" for that second is lying to the operator, and a liveness check that believes the callbacks never fires on a dead mic at all (V2). |
| `principal` | [`Principal`](#principal) | yes |  | Who started this session: `device` for a deck `ptt` edge, else the caller of `voice.listen` (§7.4). |
| `session` | [`SessionId`](#sessionid)? | no | `null` | The **session** that caller was on, which `principal` alone does not give (B2). §4.9 requires both on every `op.answer`/`op.order` the dispatch derives, and ownership in this daemon is per session and never per principal (§4.2) — so without it a dictated answer could not be attributed to the connection that asked for it, and an `agent.wait` on the owning session could not be handed the result. `None` for a deck edge, which has no session at all. |
| `since` | [`Ts`](#ts) | yes |  |  |
| `utterance` | string | no | `""` | The id every transcript of this session carries (`ut_<n>`, §4.12's `transcript.partial\|final{utterance}` and B12's `order.dispatched`). Allocated when the capture opens, because the partials start arriving before anything else has a chance to name them. |

### `Reply`

| field | type | required | default | description |
|---|---|---|---|---|
| `announce` | string? | no |  |  |
| `at` | [`Ts`](#ts) | yes |  |  |
| `redacted` | boolean | no |  |  |
| `text` | string? | no |  | Content. |

### `ReplySet`

| field | type | required | default | description |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | yes |  |  |
| `announce` | string? | no | `null` |  |
| `lane` | string? | no | `null` |  |
| `message` | [`MessageMeta`](#messagemeta)? | no | `null` | A stable provider item id updates one message as it streams. |
| `speak` | boolean | no | `false` |  |
| `text` | string | yes |  |  |

### `Route`

§7.2's output route state machine.  B7: the variant was `Sco` where §7.2 says **`ScoOurs`**, and there was no `Reopening` at all — so the state the machine spends the two and a half seconds after every press in (V1: A2DP resume is **2307 ms median**) could not be named, and `voice.route` could not report it. The distinction `ScoOurs` draws is the whole point of the machine: SCO because *we* opened the mic is a state we leave on the release, and SCO because some other app did (V7: the deck is the system default input here, so this is the routine path and not an edge case) is a state we can only wait out.

- string
- string — The output is open at 44.1 kHz and stays open (§7.2).
- string — Our own PTT capture: lanes held, output closed.
- string — Someone else's capture flipped the rate under us.
- string — Between our capture and A2DP again. V3: it opens at the device's **current** nominal rate immediately (48 ms to the first render callback) instead of waiting for 44100 (2591 ms) — a 54× difference on every post-PTT announcement, and the 2.3 s is a fixed macOS SCO-teardown timer that cannot be hurried.
- string — Two failed reopens (§7.2): the output is open at the wrong rate and resampled, `voice.route{stuck:true}`, recovering on the next rate change.
- string — `[voice] fallback_output = "system_default"`: not the deck at all.
- string — The BLE voice link (`[voice] transport = "ble"`): the deck's speaker characteristic is open, at 16 kHz, and stays open. The settled state of that transport, as `A2dp` is of a Classic deck.
- string — Our own PTT capture over the BLE voice link: the deck's speaker is closed for the press (half-duplex: no echo cancellation, V5) and reopens on the release. The BLE transport's `ScoOurs`.


### `Run`

A provider-reported background run. Counts are workers, not guessed plan steps.

| field | type | required | default | description |
|---|---|---|---|---|
| `completed` | uint32 | yes |  |  |
| `elapsed_ms` | uint64 | yes |  |  |
| `id` | string | yes |  |  |
| `name` | string | yes |  |  |
| `status` | [`ActivityStatus`](#activitystatus) | yes |  |  |
| `tokens` | uint64 | yes |  |  |
| `total` | uint32 | yes |  |  |

### `ServiceState`

| field | type | required | default | description |
|---|---|---|---|---|
| `degraded` | string? | no |  |  |
| `listener` | [`Listener`](#listener)? | no | `null` | Set while `degraded` is `port_in_use`/`hooks_hijack_risk`: the process the port belongs to, for `service.degraded` and `service.info` (§4.1, §4.12). |
| `port` | uint16 | yes |  |  |
| `version` | string | yes |  |  |

### `SessionControls`

| field | type | required | default | description |
|---|---|---|---|---|
| `editable` | boolean | no | `false` |  |
| `effort` | string | yes |  |  |
| `model` | string | yes |  |  |
| `models` | [`ModelOption`](#modeloption)[] | no | `[]` |  |
| `note` | string | no | `""` |  |

### `SessionId`

Type: string.


### `SessionInfo`

Where the agent runs (content, except `key`).

| field | type | required | default | description |
|---|---|---|---|---|
| `cwd` | string? | no |  |  |
| `key` | string? | no |  | `"claude:<session_id>"`, `"myapp:<id>"`: the idempotency key. |
| `pid` | uint32? | no |  |  |
| `pid_kind` | [`PidKind`](#pidkind)? | no |  |  |
| `terminal` | [`Terminal`](#terminal)? | no |  |  |

### `SessionSet`

Owner-authenticated settings report. Omit `request_rev` for ordinary refreshes.

| field | type | required | default | description |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | yes |  |  |
| `controls` | [`SessionControls`](#sessioncontrols) | yes |  |  |
| `request_rev` | uint16? | no | `null` | Confirm the exact `session.settings_requested` revision after applying it. Report the actual settings and a nonempty note if the change failed. |

### `SessionState`

§6.3.

One of: `absent`, `linked`, `await_hello`, `syncing`, `live`, `degraded`.


### `SetKey`

- string
- string — How big the deck draws a document it was sent: 0 small, 1 medium, 2 large. The **host** has to know it, because the host is what wraps the text — a size the two sides disagreed about would break every line in a different place.


### `Settings`

The `set` keys the host persists and restores on `boot:1`.

| field | type | required | default | description |
|---|---|---|---|---|
| `announce` | uint16 | no | `1` |  |
| `arcadesound` | uint16 | no | `0` | 0 off, 1 effects only, 2 music and effects; changed in the Arcade menu. |
| `autojump` | uint16 | no | `0` |  |
| `brightness` | uint16 | no | `5` |  |
| `chime` | uint16 | no | `1` |  |
| `demo` | uint16 | no | `1` |  |
| `idlehome` | uint16 | no | `120` |  |
| `mic` | uint16 | no | `6` |  |
| `speaker` | uint16 | no | `6` |  |
| `textsize` | uint16 | no | `1` | 0 small, 1 medium, 2 large. The **host** wraps documents with the face this picks, so it is not just a device preference. |

### `Slot`

Device slot 1..=8.

Type: uint8.


### `Source`

One of: `hook`, `api`, `mcp`.


### `Speaking`

| field | type | required | default | description |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid)? | no |  |  |
| `lane` | string | yes |  |  |
| `utterance` | string | yes |  |  |

### `StepStatus`

One of: `pending`, `in_progress`, `completed`.


### `SttInfo`

| field | type | required | default | description |
|---|---|---|---|---|
| `engine` | string | yes |  |  |
| `locale` | string | yes |  |  |
| `on_device` | boolean | yes |  |  |

### `Template`

One of: `agents`, `omarchy`, `macropad`, `usage`, `settings`, `arcade`, `system`.


### `Terminal`

| field | type | required | default | description |
|---|---|---|---|---|
| `inbox` | string? | no |  | The session's own message inbox: the Unix socket Claude Code binds for cross-session messaging (`CLAUDE_CODE_MESSAGING_SOCKET`), captured from the hook helper's environment. An agent with one is ordered by posting to it rather than by typing into its terminal (§5.5), which needs no focus, no AppleScript and no TCC grant. The per-session token that rides beside it is **not** here: it stays inside the daemon (`adapters::rt::Adapters`), never in a snapshot. |
| `iterm_session` | string? | no |  |  |
| `program` | string? | no |  |  |
| `term_session` | string? | no |  |  |
| `title_stamp` | string? | no |  |  |
| `tmux_pane` | string? | no |  |  |
| `tty` | string? | no |  |  |

### `TitleSource`

- string — `session_name` / `name`: state.
- string — Derived from the cwd: state.
- string — The first 64 B of the operator's prompt: content.
- string — A tool input head: content.
- string — Set through `agent.update`: state.


### `Tone`

One of: `blue`, `amber`, `green`, `red`, `gray`.


### `Ts`

Type: string.


### `TtsInfo`

| field | type | required | default | description |
|---|---|---|---|---|
| `engine` | string | yes |  |  |

### `Usage`

| field | type | required | default | description |
|---|---|---|---|---|
| `at` | [`Ts`](#ts)? | no |  |  |
| `items` | [`UsageItem`](#usageitem)[] | yes |  |  |
| `period` | string | yes |  |  |

### `UsageItem`

| field | type | required | default | description |
|---|---|---|---|---|
| `account` | string? | no |  | The account these windows belong to, when the source identifies it. |
| `name` | string | yes |  |  |
| `percent` | uint8 | yes |  | 0..=100. |
| `reset` | string | yes |  |  |
| `reset_tip` | string? | no |  | An expiring redeemable reset reminder; absent when none is due. |
| `tone` | [`Tone`](#tone)? | no |  | `None` derives the bar tone from `percent`. |

### `VoiceInput`

Which microphone a PTT session opens (§4.9 `voice.listen{input}`, §7.1's `fallback_input`).

- string — The deck's own input UID — the only one a deck `ptt` edge ever means.
- string — The Mac's default input device.


### `VoiceListen`

`voice.listen` (§4.9): the GUI/API entry to the same §7.4 machine the deck's `ptt` edges drive — with **the caller's** principal and session on everything it derives, never `device` (B2).

| field | type | required | default | description |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid) | yes |  |  |
| `card` | [`CardId`](#cardid)? | no | `null` |  |
| `ctx` | string | no | `"talk"` | `"talk"` or `"answer"`. A string and not a [`crate::PttCtx`] because §4.9 admits exactly those two: `name` is a device edge and an API caller may not synthesise it. |
| `input` | [`VoiceInput`](#voiceinput) | no | `"deck"` | `"deck"` (default) or `"mac"` — §4.9's `input`. |
| `max_s` | uint32 | no | `60` |  |

### `VoiceSay`

`voice.say` (§4.9).

| field | type | required | default | description |
|---|---|---|---|---|
| `agent` | [`AgentId`](#agentid)? | no | `null` |  |
| `interrupt` | boolean | no | `false` | §4.9: cannot pre-empt a higher lane; the mixer decides. |
| `lane` | string | yes |  |  |
| `text` | string | yes |  |  |
| `ttl_s` | uint32? | no | `null` | §4.9's `ttl_s?:30` — `chatter` is dropped if it has not started by then. Carried to the mixer rather than dropped here: a caller that asked for a deadline and silently did not get one is worse than one that was refused. |
| `voice` | string? | no | `null` |  |

### `VoiceState`

| field | type | required | default | description |
|---|---|---|---|---|
| `input_uid` | string? | no |  |  |
| `output_uid` | string? | no |  |  |
| `permissions` | [`Permissions`](#permissions) | yes |  |  |
| `ptt` | [`PttInfo`](#pttinfo)? | no |  |  |
| `route` | [`Route`](#route) | yes |  |  |
| `speaking` | [`Speaking`](#speaking)? | no |  |  |
| `stt` | [`SttInfo`](#sttinfo) | yes |  |  |
| `transport` | [`VoiceTransport`](#voicetransport)? | no | `null` | The transport the audio engine was built for (`[voice] transport`); null until the engine reports it, and in a build with no engine. |
| `tts` | [`TtsInfo`](#ttsinfo) | yes |  |  |

### `VoiceTransport`

Which way the deck's audio travels (`[voice] transport`): the deck's own GATT audio service on its BLE link (`deckd::ble`), or the pre-Rev-A Classic HFP/A2DP device pair addressed through CoreAudio.

- string
- string — The Rev A firmware is no longer a Bluetooth Classic audio device, so this is the default.


### `Window`

| field | type | required | default | description |
|---|---|---|---|---|
| `title` | string | yes |  |  |
| `tone` | [`Tone`](#tone)? | no |  | Absent draws gray. |
