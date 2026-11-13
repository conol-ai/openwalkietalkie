# Deck protocol (HID channel 5)

The wire contract between a host daemon and the Agent Command Deck firmware.
The firmware side is `src/deck/proto.rs` (framing, parsing, encoding) and
`src/deck/json.rs`; the vocabulary is `src/deck/msg.rs`. `tools/deck-sim.py`
is a reference host that speaks everything below.

## Transport

The device is a HID device (`Cohdl` / `OpenWalkieTalkie`, VID `1209`,
PID `5754`; BLE PnP ID `02 09 12 54 57 01 01`) with one vendor collection
(usage page `FF00`): report id 6, 63-byte input and output reports. That is
the deck's whole HID identity on both links: it is never a keyboard. The
SF32LB52x USB block has one IN endpoint besides the control endpoint, so a
keyboard interface of its own cannot exist, and a keyboard collection beside
the vendor one would make macOS class the deck as a keyboard that no ordinary
program may open (Input Monitoring). A Macropad key's keystroke is typed by
the host instead (`macro` below). It is reachable over BLE (HID over GATT) on every board and, on
Rev A, over the USB-C cable as well: the same collection, the same framing,
with the report id travelling as byte 0 in both directions. The Embassy
firmware answers a request on the link it arrived on and sends its own
events to the link whose host has spoken on that channel, USB first;
`deckd` prefers the USB instance when both are enumerated. Byte 0 of
every report body is the *channel*; the deck is channel 5.
Channel 2 is the device's system RPC (below), 4 diagnostics; channel 7 is
the voice link over the cable (the audio service's frames, one a report:
see "Voice over USB" below), which the Rev A firmware serves on its USB
link only; channel 9 is
the bootloader's recovery protocol, which only exists while the Rev A deck
is in recovery mode (the same USB device, served by `native/bootloader`; see
`tools/flash-usb.py`); a daemon must leave every other channel alone and
must ignore input reports whose channel byte is not 5 (or 7, if it speaks
voice over USB).

```
output / input report:  [6, 5, len, payload[0..len], zero padding]   len <= 61
```

A message is one line: a compact JSON object in raw UTF-8 terminated by a raw
LF (`0x0A`). `\uXXXX` escapes (including surrogate pairs) are accepted but not
needed; raw UTF-8 is shorter. A line is at most **1024 bytes including the
LF**. Lines are a byte stream over the reports: several short lines may share
a report, a long line spans several, and a line may end in the middle of a
report. The device reassembles by LF only. Empty lines are ignored.

A line longer than 1024 bytes is discarded up to its LF and answered with
`err why:size`. Keep window titles and card texts within the capacities below
and a line never gets near the limit (the longest possible `ws` is about
460 bytes, `card` about 400).

Fragments of one channel-5 line are never interleaved with another channel-5
line, in either direction. They may be interleaved with reports of other
channels at report granularity.

### Pacing and coalescing

* Send at most one output report every **8 ms** (or use write-with-response).
  The native firmware queues 64 messages (reports, input and diagnostics
  share the queue) and drops the link when it overflows; a full sync sent as
  fast as BLE allows will disconnect it. The Embassy firmware back-pressures
  the link instead of dropping lines.
* Coalesce updates: at most about **4 `agent` updates per second per agent**
  (spinner titles), and send `ws` only while an Omarchy face is visible
  (`face` tells you which face is on screen, `hello.faces` what the strip is).
* The device sends its per-edge events (`key`, `ptt`, `macro`, `answer`,
  `channel`, `snooze`, `face`) as one report each (PTT with a press identity
  and card, or an aborted release, may take two), 4 ms apart at most over BLE and back to back over USB,
  where the 1 ms interrupt endpoint and the host's own queue pace them.

### Robustness rules (what the device does with what you send)

* The whole line is validated before any of it is applied; a bad line changes
  nothing.
* Unknown top-level fields are ignored; an absent optional field keeps the
  value the device already has (an `agent` line with only `title` is a title
  update).
* Strings are cut to the field's capacity on a character boundary. Invalid
  UTF-8 bytes become U+FFFD, a lone surrogate too; C0 control characters and
  DEL become a space. The host should still truncate first: a 1024-byte line
  is the hard limit.
* Numbers are non-negative integers. Booleans may be `true`/`false` or `1`/`0`.
* Rejections are answered with `err`, rate-limited to one per second:
  `parse` (not a valid message: malformed JSON, missing required field, wrong
  type, bad escape, `hm` not shaped `HH:MM`), `range` (an id, index, count or
  value outside its range, `hm` past 23:59, or a `card`/`reply` for an agent
  the device does not have),
  `unknown` (an `m` or an enum member the firmware does not know), `size`.

### System RPC (channel 2)

One JSON request at a time, framed `[6, 2, len, payload]` like channel 5 but
reassembled as a single `{…}` object (a fresh `{"method"` drops a stale
partial); the reply is one LF-terminated object on the same link, the request's
`id` echoed verbatim (`null` when absent).

| Method | Params | Result |
|---|---|---|
| `sys.version` | — | `{"version": "<fw>-openwalkietalkie", "board": "Rev A" \| "Rev B" \| "LilyGo"}` (the board the image was built for; `tools/flash-usb.py` refuses an image for another) |
| `device.status` | — | `{"version", "battery" 0..100, "is_charging"}` |
| `sys.reboot` | `mode?` `recovery` | `{"mode": "recovery"/"normal"}`; the reply goes out, then the deck reboots (into the bootloader's USB recovery with `recovery`, Rev A) |

Anything else is answered `{"id":…,"error":{"code":-32601,"message":"Method not found"}}`.
The device never sends unsolicited traffic on channel 2.

## Session

```
host                                   device
 |-- hello v sid hm ------------------->|   on open, then every 5 s
 |<-- hello v fw sid boot rx faces -----|   reply to every hello
 |   device sid == mine and != 0 ? done : full sync:
 |-- agent / card / reply / ws / macro / usage [/ faces] ... -->
 |-- synced ---------------------------->|
```

* `sid` is a random 32-bit session id chosen once per daemon start. The
  device's reply carries the sid it holds a **complete** state for: `0` after
  boot, after a link drop or a new sid, and until your `synced` arrives. A
  reply whose `sid` is not yours means: perform a full sync, then send
  `synced`. Every later hello with the same sid is a heartbeat plus clock.
* On a new sid the device marks what it had as stale but keeps drawing it
  (selection survives a daemon restart); `synced` purges what the sync did not
  refresh. During the sync no toasts or auto-jumps fire; a `card` raises a
  notice only if its (agent, id) is new.
* No hello for 15 s, or a link drop, makes the device show the host as
  offline (roster kept, gray) until the next hello. On a link drop release
  every key you synthesised and cancel any open PTT utterance. After a hello
  gap with the link up, the releases made meanwhile are not sent: if something
  was held, the reply to your next hello is followed by `up`.
* `boot:1` in the device's hello means its strip and settings are still the
  power-on defaults: push your persisted `faces` and `set` values. With
  `boot:0` adopt the device's `faces` list instead and persist it. After the
  hello the device is authoritative for strip and settings edits (it sends
  `faces`/`set`), the host for everything else.
* The hello does not say which deck it is. A host that keeps strips and
  settings for more than one deck may key them by the HID instance's USB
  serial number: the deck's random-static Bluetooth address as twelve
  upper-case hex digits, least significant byte first (`A1B2C3D4E5F6`),
  which the recovery bootloader reports too. Over Bluetooth HID the platform
  may list the same address most significant byte first with `:` or `-`
  between the pairs, or nothing. `deckd` does this, and sends its first
  hello only once the deck's own values are queued, so the sync is that
  deck's (`docs/host-service.md` §6.9).
* The device never sends on channel 5 before it has received a channel-5 line
  on the current connection, so nothing arrives before your first hello.
* State applied from a host line never echoes back as its mirror event (a
  `focus` does not produce a `channel`, a `set` no `set`, a `faces` no
  `faces`).

### Interface-language preferences

Capable device hellos include both `lang` (0 English, 1 Simplified Chinese,
2 Traditional Chinese, 3 Japanese) and `lang_rev` (u32). A host must not send
language messages to a deck that omits either field. The preference is
independent of `boot`: a saved device locale must not suppress restoration
of other settings or be reset by a boot-default push.

Both directions use `{"m":"language","v":0,"rev":1}`. Every explicit local
choice increments its persisted revision. A newer revision wins on reconnect;
if revisions are equal and values differ, the app wins. The host reconciles
every capable hello and acknowledges the chosen value with a language line,
even when the values already agree. The device accepts equal/newer host
revisions and persists them without echoing a language event. It sends local
language events only after this host acknowledgement; a disconnect, heartbeat
expiry or new session clears that negotiation, preserving older-host behavior.
Existing non-English device preferences without a revision migrate to revision 1;
an untouched English default remains revision 0. App choices are persisted even
without a connected deck. The API spelling is `device.set` with `k:"language"`
and `v:0..3`; the wire uses the dedicated message above, never ordinary `set`.

## Host → device

Required fields have no `?`. Text capacities are in bytes of UTF-8.

| `m` | fields | effect |
|---|---|---|
| `hello` | `v` (1), `sid` u32, `hm?` `"HH:MM"` | See *Session*. `hm` sets the header clock. |
| `synced` | | End of a full sync: purge entries the sync did not refresh. |
| `language` | `v` 0..3, `rev` u32 | Versioned UI preference and capability acknowledgement; see Interface-language preferences. |
| `system` | `cpu?`, `gpu?` 0..100; `mu`, `mt`, `df`, `dt`, `seq` u32 | Complete computer sample: memory used/total and disk free/total in MiB. Used/free must not exceed total. Omitted CPU/GPU or zero capacity means unavailable. `seq` changes on every sample, including unchanged readings. Send every 2 s in the coalescing P3 lane only when the device advertises and enables the `system` template; never send unsolicited to legacy firmware. After 10 s without a sample the deck marks the values stale. |
| `agent` | `id` 1..8, `kind?` `claude`/`codex`/`gemini`/`other`, `name?` (24), `proj?` (24), `state?` `working`/`question`/`ready`/`error`/`idle`, `title?` (64), `since?` age of the state in seconds, or `gone:1` | Upsert the roster entry; `gone` removes it (and its card and notices). `id` is the stable slot the daemon assigns (1..4 map to the Hyprland binds). `proj` is the project the session runs in (the working directory's last component): the roster is one page per project, in name order, the agents without one last on a page of their own; an agent whose `name` equals its `proj` shows its kind alone on the row, and a `title` equal to it is left blank there. |
| `card` | `agent`, `id` u16, `type` `q` (question) / `r` (ready), `text` (128), `choices?` up to 4 strings (32 each), `rec?` 0-based recommended choice, or `clear:1` | The agent's pending card. A new (agent, id) raises a notice, chime and toast; re-sending an id updates the text and choices only. `clear` resolves it (after an `answer`, or when the agent moved on). Unknown agent → `range`. |
| `reply` | `agent`, `text` (200), `rev?` u16 | Fallback text for the channel; `rev` identifies its current document. Rich conversation windows arrive in `doc` lines. |
| `session` | `agent`, `rev` u16, `field` 0..2, `page` 0..63, `model` (48), `effort` (16), `status` 0..3, `note` (96), `total` u8, `selected?` absolute index, `choices` up to 4 strings (40 each) | Requested session settings: field 0 overview, 1 model, 2 effort. Status 0 read-only, 1 ready, 2 applying, 3 failed. No unsolicited messages to older firmware. |
| `transcript` | `agent?`, `text` (200, the tail), `final?`, `p?`, `failed?`, `channel_tip?` | Live STT while PTT is held; `final:1` for the last one. Echo `ptt.p` on every partial/final. `failed:1,final:1` ends a failed transcription without sending text to the agent. Send the last ~120 bytes: the device keeps the end. Without `agent` it is the words of a session with nobody on the strip (a `ptt ctx:name`), shown for whoever is holding PTT. |
| `focus` | `agent`, `ack?` | The PC's focus changed. When the change was caused by a device `channel` event, echo its `seq` as `ack`; the device ignores a `focus` whose `ack` is older than its last `channel.seq`, and applies one without `ack` always. |
| `ws` | `active` 1-based (0 unknown) `<= count`, `count` `<= 9`, `sel` window row `< windows.length` (0 when empty), `windows?` up to 6 of `{c?, t}` with `c` `blue`/`amber`/`green`/`red`/`gray` (default gray) and `t` (48) | Omarchy face content. |
| `macro` | `f` face id 1..8, `src?` (16, e.g. the dotfile name), `keys` up to 9 of `{l, b?, k?}`: `l` label (12), `b` `left`/`right`/`enter`/`cw`/`ccw`/`shift+left`/`shift+right`/`shift+enter`, `k` keystroke (32) | Labels of the Macropad instance `f` (there may be up to three). `k` is what the host posts to the desktop when the key is pressed, `ctrl+opt+shift+cmd+<key>` with any subset of the modifiers (`alt` = `opt`) and one key of: `a`–`z`, `0`–`9`, `f1`–`f12`, `up` `down` `left` `right`, `enter` `esc` `tab` `space` `backspace` `delete` `home` `end` `pageup` `pagedown`, `minus` `equal` `lbracket` `rbracket` `backslash` `semicolon` `quote` `comma` `period` `slash` `grave`; the deck shows it as ⌃⌥⇧⌘ glyphs in the key's corner and refuses an unknown one (`err unknown`). `b` is a hardware control of the deck that presses the key. A key with neither is touch-only; missing keys, and keys with an empty `l`, draw as "+ assign" (an empty label is how a cleared slot keeps the later keys in place). Send it when `hello.faces`/`faces`/`face` shows a Macropad instance you have labels for, and again after a `macrokey`. |
| `usage` | `period` (12), `items` up to 6 of `{n (20), a? (48), p 0..100, r (24), tip? (32), c?}` | Usage face. `a` identifies the account for these percentages, displayed below the provider name; omitted on legacy reports. `tip` is an optional reminder for unused redeemable resets nearing expiry, shown below the reset countdown while connected. `c` overrides the bar tone (`<50` blue, `<80` green, `<95` amber, else red). |
| `faces` | `list` up to 8 of `{f 1..8 unique, t, n (16)}`, `t` = `agents`/`omarchy`/`macropad`/`usage`/`system`/`arcade`/`settings`/`timer` | Restore the strip (see `boot`). The list starts with `agents` and ends with `settings`, has neither in between and holds at most three `macropad`; any other list → `range`, and the strip stays as it was. A `t` the firmware predates (`arcade` before 0.6.0, `system` before 0.10.0, `timer` before 0.12.0) → `unknown`. Ignored while the face manager, gallery or naming overlay is open. |
| `name` | `text` (16) | The spoken name for a new face; applies only while the naming overlay is open (after a `ptt ctx:name`). |
| `set` | `k` `speaker`/`mic`/`announce`/`chime`/`autojump`/`brightness`/`idlehome`/`demo`/`text`/`arcadesound`, `v` | Host-side override of a setting; not echoed. Ranges: speaker/mic 0..10, announce/chime/autojump/demo 0/1, brightness 1..5, idlehome 0/30/60/120 (seconds; other values round up), text 0/1/2, arcadesound 0 off / 1 SFX / 2 music + SFX. Arcade sound changes originate in the Arcade menu and are persisted like other settings. |

## Device → host

| `m` | fields | when |
|---|---|---|
| `hello` | `v`, `fw` firmware version, `sid` held (or 0), `boot` 0/1, `rx` 1024, `lang?` 0..3, `lang_rev?` u32, `faces` list of `{f,t,n}` | Reply to every host hello. Both language fields advertise versioned language sync. |
| `key` | `k` `enter`/`cancel`/`left`/`right`/`cw`/`ccw`, `act` 1 press / 0 release / 2 step (wheel), `shift` 0/1, `f` face id | Hardware keys on an Omarchy-type face, for the host to map to compositor actions. A release carries the `shift` and `f` of its press. SHIFT and PTT never come as keys. |
| `up` | | Everything is released: sent when a press was stranded by a context change, after the hello that ends a heartbeat gap, and on overflow recovery. Release every key you synthesised and cancel any open PTT utterance. |
| `channel` | `agent`, `seq` u16 | The active channel changed by a user action or a PTT press; that agent becomes the PC's focus, and the PC echoes `focus {agent, ack: seq}`. It is not a request to raise anything on the computer: deckd brings a session's own app forward only on an explicit press (ENTER on its `Open app` card). |
| `ptt` | `act` 1/0, `agent` (0 = none), `ctx` `talk`/`answer`/`name`, `card?`, `abort?` 1, `p?` u16 | Push-to-talk edges. `talk` = dictate into the agent's prompt, `answer` = free-form answer to `card`, `name` = the name of a new face (reply with `name`). The optional nonzero `p` identifies one press; echo it on transcripts so a late result cannot replace a later recording. The release repeats the press's agent/ctx/card/p; `abort:1` means the utterance was cancelled and must be discarded. |
| `answer` | `agent`, `card`, `choice` 0-based | The user picked a choice. Confirm with `card clear` (the card shows "sent" until then and reverts after 5 s). Only sent while a host is present. |
| `snooze` | `agent`, `card` | The card went back to the queue (CANCEL on the card). |
| `macro` | `f`, `i` key 0..8, `act` 1/0/2 | Macropad key edges, by hardware binding or touch. |
| `macrokey` | `f`, `i` key 0..8, `l?` label (12), `b?` binding, `k?` keystroke | The deck's own key editor set slot `i` (with `l`, `k` when a keystroke was composed, and the `b` the slot had) or cleared it (no `l`). The deck shows the change at once; store it and send the face's whole `macro` back. May need two reports. On every `macro` press or wheel step of a key with a `k`, the host posts that keystroke (deckd: CoreGraphics, needs Accessibility); the deck itself types nothing. |
| `ws` | `go` workspace number, or `win` window row index | A workspace chip or window row was tapped. |
| `session` | `agent`, `f` 0..2, `p` page 0..63, `r` displayed revision, `i` 0..3 or 255 | `i:255` requests the overview or a picker page; otherwise applies that row from the exact displayed revision. Choices cannot cross sessions, reordered catalogs, or slot reuse. |
| `refresh` | `what`: `usage` | ENTER on the Usage face: send a fresh `usage`. |
| `language` | `v` 0..3, `rev` u32 | A local UI-language choice, emitted only after host support is negotiated. |
| `set` | `k`, `v` | A setting changed on the device (persist host-side ones: speaker, mic, announce, chime). |
| `faces` | `list` of `{f,t,n}` | The strip changed in the face manager; persist it. |
| `face` | `f` | The visible face changed (start/stop `ws` pushes, send `macro` for a Macropad). |
| `err` | `why` `size`/`parse`/`range`/`unknown` | A line was refused; at most one per second. |

Every per-edge event fits one report (`<= 60` bytes plus the LF), except PTT
edges carrying both a card and press identity, or an aborted release. These
may need two reports (at most 77 bytes of JSON plus the LF).

Booleans in device lines are `0`/`1`. Optional fields (`card`, `abort`) are
omitted when zero/false.

Arcade uses the local PTT button for Flappy Bird and emits no voice/key events
for game controls. Its default face ID is 6, inserted before Settings (ID 5);
the first four IDs are unchanged. Existing saved layouts remain valid. The new
`arcade` template requires a host build that understands it. Boot synchronization
only sends `arcadesound` when the device's reported strip includes Arcade, so
older firmware does not receive an unknown setting.

## Examples

Host lines (each followed by LF):

```json
{"m":"hello","v":1,"sid":314159,"hm":"19:42"}
{"m":"agent","id":1,"kind":"codex","name":"cohdl-api","proj":"cohdl","state":"question","title":"Run migrations on staging?","since":120}
{"m":"card","agent":1,"id":1,"type":"q","text":"Tests fail on main. Run pending migrations on staging first?","choices":["Run on staging","Skip migrations","Abort task"],"rec":0}
{"m":"agent","id":3,"title":"Compiling openmicro ⠋"}
{"m":"ws","active":2,"count":4,"sel":0,"windows":[{"c":"blue","t":"term · claude — header refactor"},{"c":"amber","t":"firefox — github · pulls"}]}
{"m":"macro","f":3,"src":"deck.toml","keys":[{"l":"build","b":"left"},{"l":"test","b":"right"},{"l":"screenshot"}]}
{"m":"card","agent":1,"id":1,"clear":1}
{"m":"synced"}
```

Device lines:

```json
{"m":"hello","v":1,"fw":"0.1.0","sid":0,"boot":1,"rx":1024,"faces":[{"f":1,"t":"agents","n":"Agents"},{"f":2,"t":"omarchy","n":"Omarchy"},{"f":3,"t":"macropad","n":"Macropad"},{"f":4,"t":"usage","n":"Usage"},{"f":5,"t":"settings","n":"Settings"}]}
{"m":"channel","agent":1,"seq":1}
{"m":"ptt","act":1,"agent":1,"ctx":"answer","card":1}
{"m":"ptt","act":0,"agent":1,"ctx":"answer","card":1}
{"m":"answer","agent":1,"card":1,"choice":0}
{"m":"key","k":"cw","act":2,"shift":0,"f":2}
{"m":"macro","f":3,"i":0,"act":1}
{"m":"set","k":"brightness","v":3}
{"m":"face","f":2}
{"m":"err","why":"range"}
```

A report carrying the first 61 bytes of a line:

```
06 05 3d 7b 22 6d 22 3a 22 61 67 65 6e 74 22 ...   (6, 5, 61, '{"m":"agent"...')
```

## Testing a daemon

`tools/deck-sim.py` (needs `pip install hidapi`, or `--dry-run` for none) does
the whole session against a device: hello and heartbeat, the full mockup sync
when the device asks for one, `--flood N` title updates at the 8 ms pace,
`--send '<json>'` for anything else, and prints every device event decoded.
`--dry-run` prints the framed reports and runs them back through a copy of
the device's line assembler, which is a quick way to check a daemon's framing
by eye.

The firmware's own tests for this contract are the inline tests of
`src/deck/proto.rs` and `src/deck/json.rs` and `tests/host/deck_protocol.rs`
(round trips of every message, fragmentation, interleaving, per-edge sizes,
escaping, and a seeded hostile-input fuzz).

## Voice (BLE audio service, Rev A Rust firmware)

The deck carries its microphone and speaker over its own GATT service on the
same BLE link as the HID device; it is not a Bluetooth audio device. The
host attaches to it with CoreBluetooth while the system keeps the HID side
(bonding, encryption and the link itself are shared). Over the USB cable
the same frames travel on report channel 7 of the HID vendor collection
("Voice over USB" below), so the two links offer the same features.
Encoding and framing live in one file, `src/audio/codec.rs`, which the
firmware and deckd both compile.

| What | UUID | Properties |
|---|---|---|
| Audio service | `e9895507-bb72-45e5-a802-ab38fe1e4bc1` | primary |
| Mic (deck to host) | `6e25df5b-b48a-4203-bb04-f3d4caeb1b7a` | notify, encrypted CCCD |
| Speaker (host to deck) | `84de7298-ab42-46e4-9c02-f6a65d990807` | write without response, write; encrypted |
| Info | `a2259f3a-8d16-4c1e-a243-78f7206e35f5` | read: version 1, codec bits 0x01, 16 (kHz), 20 (ms), features (bit 0 mic, bit 1 speaker) |

Every value is one frame; byte 0 is its kind:

| Kind | Name | Bytes after the kind | Direction |
|---|---|---|---|
| 0x01 | START | stream, codec (1 = IMA ADPCM), rate kHz (16; USB speaker also 48), frame ms (20) | both |
| 0x02 | AUDIO | stream, seq, predictor (i16 LE), step index, data | both |
| 0x03 | END | stream, next seq, reason (0 release, 1 abort, 2 limit, 3 error) | both |
| 0x10 | MIC | mode (0 off, 1 on, 2 loopback), seconds (1 to 60) | host to deck |
| 0x11 | LISTEN | on (0 or 1); USB only | host to deck |
| 0x12 | INFO | the five info bytes; USB only | deck to host |

Audio is 16 kHz mono 16-bit PCM coded as IMA ADPCM, 4 bits a sample, two
samples a data byte (low nibble first): 64 kbit/s. An AUDIO frame starts
with the coder state from before its first sample, so every frame decodes on
its own; a lost frame costs its own 20 ms and nothing after it. A full frame
is 320 samples (20 ms) in 166 bytes. When the attribute value limit is
smaller (`ATT_MTU - 3`), frames carry fewer samples
(`codec::samples_per_frame`), never a different layout. `seq` counts AUDIO
frames within a stream (mod 256), so a gap is a loss; `stream` counts
streams (mod 256) on each side.

**Microphone.** A PTT press that the deck reports to a present deck host
(`ptt act:1`) powers the PDM microphone and starts
a stream: START, then an AUDIO frame every 20 ms (the first about 90 ms
after START: the microphone powers up and its filters settle), then END. The release
ends it with `release`, CANCEL or `up` with `abort`; the UI's 60 s session
cap ends the press, so it too arrives as `release`, and `limit` is the
deck's own backstop at 62 s. A release ends the stream even if the host has
gone away meanwhile, and a release followed by a new press is END then a new
START, never one stream across two presses. The channel-5 `ptt` lines are
unchanged and still decide what the words are for; the audio only rides
alongside. Nothing is captured while nobody is listening (a subscription to
the mic characteristic, or LISTEN over the cable), and an unsubscribe or a
lost link ends a live stream with `error`. If the deck stalls long enough to overrun the microphone's 80 ms
ring, those samples are gone: `seq` jumps by 4 and the stream carries on.
MIC mode 1 starts a stream without a press (deckd's `voice.listen`,
diagnostics), for at most `seconds`; mode 0 ends it with END `abort`. A
press during such a stream takes it over: the stream carries on as the
press's, with the press's limit, its release ends it, and MIC mode 0 no
longer does (a host closing its own capture must not cut off an operator
who has just pressed). Mode 2 is the same
capture with the speaker left on, so a host can play a tone and hear it come
back through the deck's own microphone: a test of both paths, never used for
speech. Other modes are ignored.

**Speaker.** The host sends START, AUDIO frames at real-time pace (a lead of
up to about 100 ms is fine) and END when it has nothing more to say. The
first START or AUDIO frame powers the audio PLL, the DAC and the amplifier
(about 115 ms, during which frames queue up), then the deck plays through an
80 ms DMA ring at 48 kHz, so a stream starts about 0.2 s after its first
frame. Up to 128 frames (293 ms at USB 48 kHz) can wait in the deck; one that arrives to a
full queue is dropped (and logged). A late frame is covered with silence
once the ring is down to 8 ms; END, or 300 ms with nothing, ends the stream
and the amplifier goes off once the tail has played. A START for a codec,
rate or frame length other than IMA ADPCM, 16 kHz (or 48 kHz for a USB speaker), 20 ms is refused: that
stream's AUDIO is dropped until its END. The link is half-duplex
like a walkie-talkie: while a microphone stream is live the deck discards
speaker frames, because its own speaker reaches its microphone at about
+17 dB and there is no echo cancellation. The host must not speak while it
is capturing either.

**Link settings.** A 64 kbit/s stream needs the LE data length extension
(251-byte link packets); the deck requests it and the 2M PHY on every
connection, and macOS grants both (251/251 bytes, 2M both ways, ATT MTU
247). Idle, macOS runs the deck's link at 15 ms intervals with a peripheral
latency of 22, which lets the deck's radio sleep through 22 events (345 ms)
at a time: that is why a BLE HID report takes up to ~360 ms, and it stalls
a stream (a mic stream measured 345 ms gaps every 1.5 s, and the host's
writes wait as long for the deck to listen). So audio activity (a microphone stream starting,
a speaker frame arriving) makes the deck ask for 15 ms with no latency, and
5 s after the last audio it asks for the host's previous parameters back.
With the fast link the largest gap between mic notifications measured
43 to 59 ms.

**Speaker quality.** Host 16 kHz playback uses a small ADPCM trellis search
with a greedy fallback: each frame's reconstruction error can only improve
from the same initial state. This does not change the wire codec or microphone
encoder. The deck reconstructs 16 kHz speaker samples with a 127-tap polyphase
FIR (7.3 kHz cutoff, 1.3125 ms group delay), suppressing the spectral images
of the old linear interpolator. Native 48 kHz samples go directly to the DAC.
Both host output rates apply the speaker EQ.

**Voice over USB.** New firmware adds INFO feature bit 2 (`0x04`) in its
USB LISTEN response to advertise 48 kHz speaker support. Hosts select 48 kHz
only on USB with that bit and compatible INFO; missing or old INFO falls back
to 16 kHz. BLE and the microphone remain 16 kHz. At 48 kHz, the same 110-sample
USB audio frame covers 2.29 ms, about 436 reports/s and 28 KB/s including HID
framing. The 128-frame receive queue covers amplifier startup plus host lead.
The host renders 960 samples every 20 ms, keeps its pacing clock in wall time,
and includes 48 in START. On a USB/BLE switch the output reopens at the
new negotiated rate; a microphone press still closes speaker playback.

On the cable the frames above travel on report channel
7 of the vendor collection, framed like channel 5 (`[6, 7, len, frame]`)
but one frame per report, never split or joined: a frame is at most 61
bytes there, so an AUDIO frame carries 110 samples (6.9 ms) instead of 320,
in the same layout, exactly as it does over a small ATT MTU. Both
directions use it: the deck's microphone frames come in as input reports,
the host's MIC requests and speaker frames go out as output reports, with
no pacing beyond the endpoint's 1 ms polling (a speaker frame every 7 ms is
what it is for). Subscribing is a frame, not a descriptor: the host writes
LISTEN `on:1` and the deck answers with INFO, the info characteristic's
five bytes, which is the moment the link is ready; LISTEN `on:0` or the
cable going away unsubscribes, and either ends a stream bound to the cable
with `error`. A deck whose firmware predates this never answers LISTEN;
deckd gives it two seconds and then treats the cable as having no voice
link. When a host listens on both links, a stream goes to the cable (the
deck's preference, the same as for its events) and stays there until its
END; the speaker takes frames from either link. The cable needs no pairing:
the HID reports on USB are not encrypted, and the microphone is open only
to the host that holds the cable. Channel-7 reports written over the
Bluetooth HID characteristic are ignored: the audio service is the way
there.

**Pairing.** The mic and speaker characteristics require an encrypted link,
so the deck must be paired (bonded) with the Mac, which is the same pairing
its BLE HID side needs. An app that connects to an unpaired deck also brings
in macOS's HID service, which starts pairing and puts a pairing request on
screen; until it is answered, macOS holds back the app's writes to the deck
("Enqueuing a delayed command" in the Bluetooth log), so the speaker stays
silent while notifications still arrive. The `audio-open` firmware feature
(bench only) drops the encryption requirement but cannot stop that prompt.

## Rich conversation windows

`doc` carries `agent`, `d` (`r` reply, `w` workflow, `c` card, `t` transcript), `id`,
`first`, `total`, `font`, and `lines`. There are at most 17 lines of 48 UTF-8
bytes each; the host reduces the count until the encoded JSON fits 1000
bytes. The deck retains at most 40 contiguous lines. `doclines` asks for the
next window using the same agent, document kind, id and first line. A first
line of 65535 requests the tail.

Workflow is opt-in: `doclines` with `d:"w"` selects it for that agent;
`d:"r"` returns to chat. Older firmware continues to receive chat. Both views
share the device's reading buffer and the `reply.rev` envelope. The device
accepts only its selected kind and current revision, including after reconnect.
Workflow updates change revision because plan and activity rows can move.
The host coalesces superseded windows, and sends the revision envelope before
its window without allowing another revision to interleave. Missing windows
are retried after five seconds.

For chat, `richrow`, `richtext`, and `richtitle` select 25, 29, and 32 px
reading faces. Every rich line begins with a block letter (`p` paragraph,
`h` heading, `c` code, `q` quote, `r` divider, `b` blank), then an ASCII indent
digit (12 px per step). Inline style changes use U+E000 followed by `A`…U+00C0:
subtract `A` for flags bold=1, code=2, link=4, emphasis=8. Bits 4–6 select
a code-token color: keyword=1, string=2, number=3, comment=4, function=5, type=6.
Common fenced-code languages are highlighted lexically; unknown languages
keep plain monospace text. Each line starts
with flags 0. The host removes source control/style-marker characters,
parses CommonMark/GFM, and wraps against the same font metrics the firmware
uses. Code is monospaced, quotes have a side bar, links retain their URLs,
and tables reflow as text rows. This is a text display; image alt text is
shown instead of downloading images.

A conversation document keeps its id while messages stream or append.
Overlapping rich windows can merge when the total line count changes.
Pruning retained history or toggling tool details changes the id. The host retains up to 64 messages,
64 KiB per message and 512 KiB per session; older messages remain in the
provider's session. User, assistant and tool entries have separate labels
and live/done status. Each message is parsed independently so an unfinished
code fence cannot consume later messages.

Consecutive tools are grouped into a compact activity summary: “Explored”
for reads/searches, otherwise “Activity”, with a running or failed state.
Up to three distinct input summaries are shown with blue verbs and short
file/command targets. “Show details” expands the whole group's original
inputs and outputs; these stay hidden by default. A disclosure row begins
`t0` (collapsed) or `T0` (expanded), two lowercase hex digits for the first
tool's message index (00–3f), then the label. The renderer reserves 24 px
for its arrow. Tapping sends `{"m":"tool","agent":1,"id":7,"i":2}`;
the host validates revision/index and returns a new revision anchored at
the disclosure. Stale taps are ignored; expansion survives streaming and
resync. Raw output stays out of the flat `reply` fallback.

Workflow uses the same rich fonts and disclosures. Indices `40` and `41`
(decimal 64 and 65) expand the full plan and older activity; they are valid
only when present in the selected workflow document. A `w0DDTT` row draws
plan progress, where `DD` and `TT` are two-digit hexadecimal completed and
total counts (`0 <= DD <= TT <= 64`, `TT > 0`). Only valid counts render as
progress. The compact plan shows three steps around the current one;
activity shows the latest three tools plus running or expanded entries.

The channel uses all space above the footer except visible card controls.
It opens at the tail and follows the latest text while the operator is at
the end. Scrolling away requests and pins a reading window; scrolling back
to the end or Shift+Enter resumes following. Expanding a tool holds its
header in view when its details extend beyond the screen, so incoming text
does not pull the reader away. A view that still shows the end keeps following.
Codex deltas are coalesced at 400 ms. Claude transcript changes are read at
500 ms intervals while active, with hooks supplying tool and turn status.
Reasoning/thinking blocks are excluded. Transcript reads are restricted to
Claude's configured projects directory and the named session's JSONL file.

For a Claude talk session without a verified Channels connection, the host sends
an initial empty `transcript` with `channel_tip:1` and the current `p`. The device
shows “Tip: enable Channels in Mac Integrations” in the small context line while
listening and transcribing. Partials refresh the flag from channel readiness;
answer/naming sessions and other agents never show it. A new press or host
session clears the hint, and stale press IDs are ignored. The optional flag
defaults to false, so older hosts keep the existing presentation.

After a live PTT release, the transcript remains visible with a TRANSCRIBING
pill and progress dots until its matching final arrives. A fresh PTT press
starts recording immediately; older tagged results cannot change that view.
ESC cancels the pending utterance, failures show a notice, and link loss or
a 125 s device watchdog releases the waiting screen.

## Session model and thinking effort

Tap the model/effort row below the session name to open settings. Select Model
or Thinking effort, turn the wheel to choose, then press ENTER (or tap a row).
The wheel crosses page boundaries; LEFT/RIGHT also change pages. CANCEL goes
back. The current selection has a dot. The chat keeps its full reading height.

The host sends a `session` response only after a request from the device. The
single bounded picker holds four labels, while the host owns up to 64 model
options and the effort levels reported for each model. A choice names its page
revision, never an executable command. Changes stay pending until the owning
adapter confirms them; a rejected or unconfirmed change preserves the last
reported values. Catalog changes invalidate old touches. A reconnect requests
fresh state, and disconnected/read-only controls never emit a change.

Codex uses the existing shared app-server connection: `model/list` supplies the
catalog, `thread/settings/update` changes only `model` and `effort`, and
`thread/resume` reads back the result. These are session defaults for subsequent
turns; no prompt is sent and an active turn is not interrupted. The installed
CLI schema documents this method; older servers that refuse it show an update
instruction. Catalogs refresh on connection, account changes, and periodically.

Claude's status line reports the current model where available. Its view stays
read-only with `/model` and `/effort` instructions: Channels does not provide a
model/effort control API. No terminal emulator is assumed and no global model
settings are rewritten.
