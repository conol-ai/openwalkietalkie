# deckd — the OpenWalkieTalkie host service

`host/` is a Cargo workspace for the macOS side of the deck: a background
daemon that owns the Bluetooth HID link, keeps the deck's roster, cards and
settings, and answers the deck the way the firmware expects, and an API two
doors wide that everything else speaks: the gpui GUI, the Claude Code and
Codex hook adapters, scripts and voice are all clients of it, with no
privileges a `curl` one-liner lacks. The second door, HTTP and WebSocket
over TCP, is off by default, and the next release removes it (see "The API").
The design is [docs/host-service.md](../docs/host-service.md)
(its Status section says what is built), the wire contract is
[docs/deck-protocol.md](../docs/deck-protocol.md), and the device grammar is
[docs/agent-command-deck.md](../docs/agent-command-deck.md).

The current voice transport is the deck's custom BLE audio protocol; see
"Voice over BLE" below. The remaining rollout descriptions retain the
week-3 snapshot of the eight-week plan (design §15): the device link
of week 1, the open API of week 2, and the agent adapters of week 3 — Claude
Code and Codex hooks become cards on the deck. The LaunchAgent runs
`deckd run`, which owns the deck link. macOS is the supported platform;
Linux runs the service (see `host/omarchy/`), and Windows runs a minimal
build of the service and the app, below.

## Windows (experimental)

`deckd` and the companion app build for `x86_64-pc-windows-msvc` and have
been run against a Rev A on USB: the deck links, the app connects, and the
USB firmware updater has installed a release (0.9.0 → 0.13.0, 2026-11-14).

- **The local door is a named pipe**, `\\.\pipe\openwalkietalkie-deckd-<hash>`,
  the hash being the data directory's (`deck_client::windows::pipe_name_for`),
  so `DECKD_DATA_DIR` and `--data-dir` move it as they move the socket. Every
  instance is created with a DACL that grants only the daemon's user, remote
  clients are refused, the first instance must be the first of its name, and
  both ends check that the other runs as the same user. `deckd ctl` and the
  app speak to it exactly as to the socket. A peer is never `trusted` (there
  is no code-signature check yet), so allowing a permission card from the app
  is refused while `[trust] require_trusted_for_allow` holds.
- **Data** lives in `%LOCALAPPDATA%\OpenWalkieTalkie` (config, state, `logs\`,
  `caches\`). The single-instance lock is `LockFileEx` on `deckd.lock`.
- **Nothing starts it at login yet.** Run `deckd run`, or press **Start** in
  the app, which starts the `deckd.exe` beside it with no window. The app
  allows `firmware.check`/`install` only when it runs from a folder named
  `bin`, as on Linux.
- **Firmware updates** download with `%SystemRoot%\System32\curl.exe`, keep
  the computer awake while flashing, and wait up to three minutes for the
  deck to come back after each restart (a deck passed through to a virtual
  machine took 176 s). Windows binds no driver to recovery's bulk interface,
  so the transfer uses the HID path (4 min 46 s for 0.13.0 on Rev A).
- **Not built for Windows yet:** voice (BLE audio, recognisers, speech), the
  Claude channel door and the Claude inbox, the Codex app-server client,
  desktop focus and injection, launch at login, and the trusted-GUI check.
  The Claude and Codex paths log that they are unavailable and leave the
  rest of the service running; voice has not been tried. The test suite does
  not compile on Windows (several tests are Unix-only), so CI builds the
  Windows host and runs the client crate's tests only.

## Voice over BLE

Current Rev A firmware carries the deck's microphone and speaker over its
own GATT audio service: 16 kHz mono IMA ADPCM frames on BLE, alongside the
HID deck protocol. `deckd` uses CoreBluetooth for that service. The deck
does not appear as a Bluetooth Classic microphone or speaker in macOS Sound
settings, and there is no HFP/SCO or A2DP route to select.

The default is already BLE. For an existing configuration, set this in
`~/Library/Application Support/OpenWalkieTalkie/config.toml`:

```toml
[voice]
transport = "ble"
```

`transport = "coreaudio"` is the compatibility option for older Classic
firmware. `device_uid`, `mic_warm` and `speak_over_sco` describe that legacy
route; BLE uses the connected deck's GATT service and captures from its
stream START instead of warming a Classic input. Changing `voice.transport`
requires a service restart; `config.reload` reports `restart_needed`.

Pair the deck in System Settings → Bluetooth and allow OpenWalkieTalkie under
Privacy & Security → Bluetooth. The daemon attaches to the deck macOS
already holds connected for HID. Speech Recognition authorization depends
on the STT engine; strictly on-device Apple recognition has been measured
without that grant (design §7.8). The Microphone grant is needed for the
Mac's own input, which only `voice.listen` with `input: "mac"` opens, and
for a Classic deck's input on `transport = "coreaudio"`; the BLE deck
microphone uses the Bluetooth grant. A press while the deck's
microphone is unavailable gets silence and `no_device`, never the Mac's
microphone: `[voice] fallback_input` is no longer read, and an old line is
named in the log so it can be removed.

After rebuilding, replace the installed app or service binary and restart
the running app and service. `tools/build-host.sh` only builds `deckd`;
`tools/install-host.sh` rebuilds, installs and restarts the standalone
LaunchAgent. For a bundled app, `tools/package-macos.sh` produces the new
bundle; install it, reopen it, and use Service → Restart. An already running
process continues using its previous build.

```sh
deckd ctl --scopes observe voice.get
deckd ctl --scopes voice voice.test '{"what":"speaker"}'
deckd ble scan --seconds 3
# Stop the service before an active probe, then restart it afterwards.
deckd ble probe --seconds 3                 # capture the deck's microphone
deckd ble probe --seconds 3 --tone 440      # play through its BLE speaker
```

`voice.get` reports `transport: "ble"` and the BLE route. The scan and probe
commands inspect the custom service directly; a terminal probe uses the
terminal's Bluetooth grant, while the installed daemon needs its own grant.
The complete transport contract and bench status are in
[design §7.9](../docs/host-service.md#79-the-ble-transport-voice-transport--ble-the-default).

### Speaker EQ

`deckd` applies a fixed Rev A speaker preset to all mixed speech and chimes
on the 16 kHz device output, over both USB and BLE. It runs after resampling
and mono downmix, before PCM quantization and ADPCM encoding:

| Filter | Frequency | Gain | Q |
| --- | --- | --- | --- |
| High-pass (12 dB/octave) | 250 Hz | — | 0.707 |
| Bell | 1.1 kHz | −5 dB | 1.0 |
| Bell | 6.3 kHz | −6 dB | 4.0 |

The preset leaves 3 dB of headroom and clamps exceptional sample peaks at
−1 dBFS. Filter state continues across audio blocks and silence and resets
when the output reopens. It adds no buffering or dependencies.

These are conservative estimates from the supplied speaker's 0.1 W / 0.1 m
frequency-response plot, not a measured enclosure calibration. The bass
roll-off and narrow nulls are not boosted; peaks above the 8 kHz Nyquist
limit cannot be corrected on this transport. The legacy CoreAudio path and
raw `deckd ble probe` measurement tones do not use this Rev A preset.
Rebuild and restart `deckd` to apply it; listening and an in-enclosure
measurement are still needed to refine the preset.

## Crates

| Crate | Kind | What it holds |
| --- | --- | --- |
| `deck-proto` | lib, no I/O | The channel-5 vocabulary in both directions (`HostLine`, `DeviceLine`), the line encoder with the byte caps of design §6.5 (cut on char boundaries with `…`, refuses > 1000 B), the 61-byte report chunker, and the line assembler / device-line parser. Its contract tests mount the firmware's own `src/deck/{text,msg,json,proto}.rs` read-only, so every line the host can emit is run through the device's assembler and parser at every chunk size, and every event the device encodes parses back. |
| `deck-model` | lib, no I/O, no clocks | The domain: `Snapshot`, `Command` / `Event{rev}` / `Effect` exactly as design §3, the `Store` reducer `apply(Command, now) -> (events, effects)`, the card, delivery, hold and lease state machines driven by `Command::Tick(now)`, slot policy, redaction, `project(&Snapshot) -> Vec<DeviceOp>`, and the JSON Schema of everything via `schemars`. The barrier-order and byte-cap rules of §14 are `proptest` properties. |
| `deck-client` | lib | The typed async client every non-GUI consumer speaks through: `Client::connect(ClientConfig)` over the Unix socket (newline-delimited JSON-RPC) or `GET /v1/ws` (one JSON value per text frame), `session.hello` first, `call`/`call_as` matched to responses by id, `events()` as a broadcast of `Event` / `Resync` / `Reconnected` / `Disconnected`, and auto-reconnect with a 250 ms → 5 s backoff that hellos again with `since: <last rev>` so the ring replays what was missed. `default_socket()` is the daemon's socket path, `DECKD_DATA_DIR` honoured. `deckd ctl`, `deckd events` and `deckd status --live` are its first users. |
| `deckd` | bin | The service and its CLI: the `deckd-hid` thread (`DeviceView`, lanes, shadow, sync barrier, session state machine, framer) with its own `deckd-hid-write` writer, the `deckd-core` actor (applies commands, runs Device/Timer/Persist/Deliver effects, owns the sessions, holds, waiters and mailboxes), the bus (broadcast + a 4096-event ring for `since:rev` replay), `deckd::api` (the two doors, one `dispatch`, the scope table, sessions, redaction at fan-out), `deckd::trust` (the `LOCAL_PEERPID` → code-signature → path check and the ancestry cap), `deckd::tokens` (`tokens.toml`, blake3), `config.toml` with its watcher, `state.json`, `status.json`, the flock, logs, and the subcommands below. |

The firmware's copy of the demo session (`src/deck/demo.rs`) and
`tools/deck-sim.py` are the reference: `deckd device dry-run` frames the same
lines byte for byte, and the gate diffs the two.

## What works today

- **The device link.** `deckd run` finds the deck (VID `1209`, PID `5754`,
  usage page `ff00`), opens it without seizing it, says `hello` with a fresh
  `sid`, writes the sync barrier in the §6.5 order (`agent`s, then `card`s,
  `reply`s, `focus`, `macro`, `usage`, `ws`, on `boot:1` the persisted `faces`
  and all eight `set` keys, then `synced`), and keeps the heartbeat going
  every 5 s: 2 s retry, degraded after three misses, the handle reopened
  after six. A reply with another `sid` (deck rebooted, link came back)
  triggers a new barrier; card ids continue from `state.json` (`last + 1`,
  skipping 0) so the deck raises no duplicate notices. Lines go out through
  the keyed, prioritised outbox (keep-latest per key, four lanes, 1 s aging,
  a 4/s/slot title bucket, fragment-atomic chunking, the 8 ms pace floor); on
  macOS the BLE acknowledgement, not the pacer, sets the cadence at roughly
  300 ms per report while the deck is idle (spike S3). Because that write is
  synchronous, it happens on a `deckd-hid-write` thread of its own: the hid
  loop hands over one report at a time and never waits for it, so reads and
  `focus{ack}` are not queued behind a title update. A write that has not
  returned after 5 s drops the link (`write_stall`) and the thread is
  abandoned, so `SIGTERM` always completes.
- **The adapters.** A Claude Code or Codex session that has deckd's hooks
  installed takes a slot on the deck within its first event: a permission
  prompt becomes a card the operator answers from the deck, a Claude
  `AskUserQuestion` becomes one card per question, `Stop` puts the reply on
  the deck, and a session that is killed leaves it. See "The hooks" below.
- **Demo mode.** `deckd run --demo` registers the mockup roster (the same
  data as `tools/deck-sim.py`) through the store's own API as an in-process
  virtual agent and reacts like the reference host: an `answer` puts the
  agent to work, `refresh` nudges the usage bars, a PTT release types a
  transcript and a reply, naming a face over PTT names it "Deploy". It is
  how the deck is exercised with no CLI running.
- **Coexistence.** The daemon never writes on channel 2 (the device's
  system RPC), and IOKit delivers every input report to every non-seizing
  client (spike S3), so the bench tools keep working next to `deckd`.
- **The CLI.** `run`, `status`, `ctl`, `events`, `token`, `hook`,
  `integrations`, `doctor` and `device flood|sync|metrics|dry-run`, below.

## The API

Week 2 made the daemon a service other programs use. Everything the deck
shows — the roster, the cards, the answers, the settings, the metrics — is
one JSON-RPC 2.0 surface over a single snapshot with a monotonic `rev`, and
every consumer is an ordinary client of it: the GUI when it lands, the hook
adapters of week 3, `deckd ctl`, a shell script, a Python loop. Nothing
has a private channel into the daemon, so a third party can be an agent on
the deck — take a slot, ask a question, receive the operator's answer —
without the daemon knowing anything about it, and the same call from the same
principal behaves the same whichever door it came in by. Every connection is
a **session**, an agent is owned by `{principal, session}`, and an answer is
delivered to the owning session before the card is cleared on the deck
(design §4.8), so "cleared" on the deck means the agent has it.

**Two doors, one `dispatch`.**

| Door | Address | Who you are | What it speaks |
| --- | --- | --- | --- |
| Unix socket | `~/Library/Application Support/OpenWalkieTalkie/deckd.sock` (dir 0700, socket 0600; `DECKD_DATA_DIR` moves it) | peer credentials: the uid must be the daemon's own; the pid goes through the trust check of design §4.2 (`LOCAL_PEERPID` → code signature → bundle path), which is the only thing that ever sets `trusted` | newline-delimited JSON-RPC, one request or batch per line, `session.hello` as line 1 (anything else is `-32001 {why:"hello_first"}` and the socket closes); events arrive as `event` notifications, a lagged reader gets `resync`; a line over 256 KiB is `-32012` and closes |
| TCP (off by default, below) | `http://127.0.0.1:47201` (`[api] port`; `0` binds an ephemeral port, and the bound port is written to the data dir's `port` file either way), only while `[api] tcp = true` | `Authorization: Bearer dk_…`, verified against `tokens.toml` (blake3 hashes, expiry honoured, mode 0600); the token's name is the principal and its scopes are the maximum. `trusted` is never true over TCP | `POST /v1/rpc` (one request or a batch; notifications alone answer `204`), `GET /v1/ws` (one JSON value per text frame, the same hello-first session protocol as the socket), `GET /v1/events` (SSE, `since=<rev>`, `types=`), and the read-only mirror `GET /v1/state`, `/v1/agents`, `/v1/agents/{id}`, `/v1/cards`, `/v1/device`, `/v1/voice`, `/v1/schema` |

**The HTTP/WebSocket door and its tokens are off by default, and the next
release removes them.** Everything shipped with the deck speaks over the Unix socket
— the app, the Claude Code plugin's hooks, `deckd ctl`, the Omarchy bridge —
and so do the connected apps such as Conol; nobody else is known to use the
TCP door or a bearer token. So `deckd run` binds no TCP port unless
`config.toml` asks for one, and the next release deletes the door and its
tokens together with `op.*`, the public `voice.*` methods, `focus.report`
and the socket's `agent.wait`, none of which Conol or the bridge calls. To
turn the door on until then, set

```toml
[api]
tcp = true
```

and restart the service, or switch **Service → Local API** on in the app,
which writes the same key and restarts it; `tcp = false` keeps it off. A
file that leaves the key out gets the door only while the hooks deckd gave
Claude Code before its integration became a plugin may still post to it.
Either of two things says so:

- Claude's `settings.json` (under `$CLAUDE_CONFIG_DIR`, else `~/.claude`)
  still carries one of those `type: "http"` hooks. Only the HTTP hooks
  count: other leftovers ask for Repair too, but post nothing. The
  Integrations page's Claude row asks for **Repair** (`deckd integrations
  install claude`, which migrates them), the app's Local API switch offers
  that Repair before it turns the door off, and the start after the repair
  leaves the door off.
- deckd's own record of the Claude target (`integrations.json` in its data
  directory) is still a pre-plugin one. That is how hooks written under a
  `CLAUDE_CONFIG_DIR` the service does not see keep their door. Nothing on
  the Claude row shows them, so migrate them from a shell that sees them:
  `CLAUDE_CONFIG_DIR=<that folder> deckd integrations install claude`. Any
  Claude install, repair or removal replaces the record, and with it this
  exception, as an explicit `[api] tcp` does.

`deckd run` logs which rule applied as it starts (`tcp door: [api] tcp is
left out, and the door is off by default`, …), `config.get` publishes the
result as `api.tcp` and the rule as `api.tcp_rule` (`explicit`, `default` or
`legacy_hooks`; not a key of the file, so `config.set` refuses it), and
`deckd doctor`'s `tcp` row calls a door that is off "off", not "degraded".

While the TCP door is on, `GET /v1/info` is the only route without a token —
it is how a client learns the daemon's version, socket path, port and
`degraded` reason. Everything else on the door passes three checks before it
is dispatched: `Host` must name
loopback on the port the daemon actually holds (else 400 `bad_host`), an
`Origin` header that is not in `[api] allowed_origins` is 403
`origin_forbidden` on every route, the WS upgrade and `/v1/info` included, and
a body over 1 MiB is refused (`-32012` for `/v1/rpc`). If the bind fails the
daemon keeps serving the socket and the deck, reports
`service.degraded{reason:"port_in_use", listener:{pid, command}}`, retries
every 30 s, and probes the squatter: an answer on 47201 that is not ours turns
the reason into `hooks_hijack_risk`. `curl --unix-socket` is not supported (the
socket is not HTTP); `deckd ctl` is the shell client for that door.

The CLI under "Running it" below is the shell face of this API and nothing
else: `deckd ctl` is one call (`--scopes` asks for what it needs, exit 3 means
`-32002 need:"trusted"`), `deckd events` is a subscription printed as JSON
lines, `deckd token` mints and rotates the bearer tokens the TCP door checks
(they open nothing while the door is off), `deckd status --live` adds
`service.info` to the `status.json` it already prints, `deckd device
flood|sync|metrics` are `admin` calls, and `deckd doctor` checks both doors
(socket path length, a `session.ping`, `/v1/info` being ours and not a
squatter's or the door being off, `tokens.toml` at 0600). All of them go
through `deck-client`, so they hit exactly the code a third party's client
does. So do the care verbs, one for everything the Mac app does for the
deck through the service, each calling the method the app's control calls
and printing its reply as it came with `--json`: `deckd care …` (the setup
checks, the footprint, the sources and their pause, the Usage face's
sources, the tests, the permissions, the decks this Mac keeps a setup for,
the two updates),
`deckd firmware status|check|install`, `deckd config get|set|reload`,
`deckd service restart|stop` and `deckd device get|set|faces|set-faces|set-macro`
(§10.7 of `docs/host-service.md` has the table).

**Scopes.** A call carries a principal (`uid:501` over the socket, the token's
name over TCP) and a scope set: `observe`, `observe:content`, `observe:raw`,
`agent`, `operate`, `voice`, `device`, `hooks:ingest`, `admin`. `session.*`
needs none; reading (`state.*`, `agent.list|get`, `device.get`, `voice.get`,
`service.info`) needs `observe`; being an agent (`agent.*`, including
`agent.wait`) needs `agent`; answering somebody's card (`op.*`) needs
`operate`; `voice.*` needs `voice`; the `device.*` setters and `focus.report`
need `device`; `config.get|reload`, `token.*`, `metrics.get`, `log.tail` and
`device.flood|sync` need `admin`. A session asks for the scopes it wants in
`session.hello` and is refused `-32002 forbidden{need}` if that is more than
its maximum, rather than being handed a silent intersection — over the socket
the maximum is `[api] uds_max_scope` (`admin` by default; the default ask is
`[api] uds_default_scope`, `observe`), over TCP it is the token's own list,
and a client that wants whatever it can get omits `scopes`. Content — card
text and choices, titles taken from prompts, transcripts, replies, `meta` —
is redacted per subscriber at fan-out for anyone without `observe:content`
(`operate` and `admin` imply it; `observe:raw` is never implied and has to be
named), except that a session always sees its own agents whole. Two limits
are not scopes: a peer whose own process or ancestry is an agent CLI
(`claude`, `codex`, `gemini`, `node`, by process name or by executable path,
which is what names Claude Code's native install, `…/claude/versions/<v>`)
is capped at `observe,agent,voice,hooks:ingest` and asking past that is
`-32002 {reason:"agent_ancestry"}`; and while `[trust] require_trusted_for_allow`
holds, an *allow* on a permission or elicitation card, an `op.order` that would
be injected, and `voice.listen{ctx:"answer"}` on such a card are refused
`-32002 {need:"trusted", how:"gui_confirm"}` from anyone but the deck and the
code-signed app, with `op.confirm_requested` put on the bus so the GUI can
offer its sheet. Deny and snooze are never guarded, and a card a script set
itself is not a permission card, so scripts answer their own cards freely.

**Writing a virtual agent.** `host/examples/virtual_agent.py` is the whole of
it: 50 lines of Python over `POST /v1/rpc` that `session.hello`s, registers
`caps:{answer:"poll", order:"poll"}` with a stable `key`, sets a `q` card, and
parks in `agent.wait` until the operator answers on the deck. It needs the
TCP door on (`[api] tcp = true`), and goes with the door in the next release;
the gate switches the door on for its own daemon and runs it end to end on
every run until then, so it cannot rot. The rules it follows are design
§4.15: pass the same `client:{name}` on every plain-HTTP call so they share one
logical session (a different name under the same token is a different session,
so registering the same `key` from it is `-32004 conflict{owner}`, and
`agent.claim` is the only takeover path), use `poll` there (`push` is
`-32009` — it needs
a connection to push down), heartbeat inside the lease, and take the answer
from `agent.wait`, which is what clears the card on the deck. Over the socket
or WS the connection *is* the session, the heartbeat is implicit, and `push`
delivers `agent.answer`/`agent.order` as notifications. Design §4.14's
one-liner, after a one-time `export DECKD_TOKEN=$(deckd token create --name
build-bot --scopes observe,voice)` kept by the user (nothing is written to
disk by the daemon), is the smallest client there is — while the TCP door is
on, which it is not by default (`[api] tcp = true` and a restart, or
**Service → Local API**), and only until the next release, which deletes the
door, the tokens and the public `voice.*` methods this one calls:

```sh
# Needs the TCP door: [api] tcp = true (it is off by default), then restart deckd.
curl -s -H "Authorization: Bearer $DECKD_TOKEN" -d '{"jsonrpc":"2.0","id":1,"method":"voice.say","params":{"text":"build finished","lane":"chatter"}}' http://127.0.0.1:47201/v1/rpc
```

With the door off nothing listens on 47201 and `curl` cannot connect. (In
the historical week-3 slice, that call reaches its rate limit and its guard
and then answers `-32013 degraded{reason:"voice_unavailable"}`: the envelope
is real, the engine is week 4.)

## The hooks

Week 3 made the CLIs agents. A Claude Code or Codex session reaches the
daemon through its own hook mechanism, deckd turns the event into the same
`agent.*` primitives a Python script would use, and the deck shows the
result. The adapters have no private path either: they are owners like any
other, one internal session each, speaking §4.6.

**One helper, two doors onto one handler.** Both CLIs run `deckd hook
<claude|codex>`, which reads the event on stdin and calls the JSON-RPC method
`hooks.<cli>` over the socket; `POST /v1/hooks/<cli>` with a `hooks:ingest`
bearer, on the TCP door while it is on, is the same handler for a third
party's own http hooks and for the pre-plugin Claude hooks that keep the
door open until they are migrated. A report-only
event (`UserPromptSubmit`, `Stop`, `Notification`, `PostToolUse`,
`SessionEnd`) is answered **before** the work is done — those hooks are
synchronous with a 2 s budget, so the operator's prompt never waits on the
roster — while a permission or question event is *held*: the request stays
open, the card goes to the deck, and the answer is written back as the CLI's
own hook output. A hold lasts `[hooks.<cli>] hold_s` (120 s) at most, always
under the 130 s the installed hook allows, and `[api] max_held_hooks` (64)
bounds how many can be parked at once; past that a hook fails open rather
than queueing.

**Claude voice messages can use Channels in any terminal.** In the Mac app,
open **Integrations → Claude voice messages**. Install or Repair the Claude
plugin, then copy the launch command into your project's terminal:

```sh
claude --dangerously-load-development-channels plugin:openwalkietalkie@openwalkietalkie
```

To keep an existing conversation, exit it first and append `--resume` followed
by its session ID. Accept Claude's local development confirmation, enable the
MCP server if prompted, and allow its `acknowledge` connection check. Press
**Re-read** in Integrations to see the session become **Channels ready**.
The bridge runs inside the existing `deckd` binary; no Node or Bun install is
needed. This applies to both dictation and messages typed in the deck app.

[Claude Channels](https://code.claude.com/docs/en/channels-reference#test-during-the-research-preview)
are a research preview. Our custom plugin requires this per-session flag and
interactive confirmation; the flag does not override organization policy or
Claude's tool permissions, and does not work in `-p`/SDK mode. Team/Enterprise
administrators must enable Channels. Restart without the flag to turn it off.
If the connection check stalls, inspect `/mcp`, then ask Claude to run
OpenWalkieTalkie's `check_connection` tool to retry it.

The plugin declares a stdio MCP channel in `.mcp.json`. The helper connects to
`<data>/claude-channel.sock`, a local socket restricted to the same user and
`deckd` executable. The daemon derives the Claude PID from the helper's actual
process ancestry. Both ends validate the current conversation against Claude's
session registry; `/clear` and resume invalidate the old connection proof.
Only the core's authorized operator orders can enter this route. There is no
new public message-ingress API and no channel permission relay.

**The cross-session inbox remains the fallback.** Every Claude hook captures
its session's inbox socket and authentication token. Until a channel event
has been acknowledged, or if the bridge becomes unavailable before writing,
orders use that inbox without focusing a terminal or requiring Automation
permission. Claude may label fallback messages as coming from another Claude
session. An uncertain channel write reports `channel_delivery_uncertain`
instead of retrying through the inbox, to avoid duplicate commands; a changed
conversation reports `channel_session_changed`. A session with neither route
gets `order.failed{reason:"no_route"}`, which the deck shows; the Ghostty
injector that used to type into such a session is gone, and with it the
Automation grant. Inbox tokens remain inside the daemon, absent from
snapshots, events and logs.

**Hand-off.** The hook helper reports the macOS bundle identifier of the app
each session runs in (`terminal.app`: Zed, Ghostty, Terminal, iTerm2, VS Code,
Claude Desktop), found from the process ancestry above the CLI or, under
`tmux`, from `__CFBundleIdentifier`. ENTER on a session's **Open app** card
brings that app forward if it is running (LaunchServices, never a launch);
a channel change or a PTT press raises nothing. A hand-off that cannot happen
says so on the deck.

`integrations.status` includes `claude_channels: [{session, state}]`, where
`state` is `checking` or `ready`. It is a connection check at the time of the
read, not an acknowledgement that Claude has acted on a particular message.

**Codex has a second, better source than hooks.** From codex-cli 0.161 the
TUI runs through a shared local app-server daemon, and `deckd::codexd`
attaches to its control socket (`$CODEX_HOME/app-server-control/`, a
WebSocket): every loaded thread is an agent — including the ones that
started before deckd did — `turn/started`/`turn/completed` are its state,
the server's own approval and `requestUserInput` requests are its cards, and
an order is `turn/steer` while a turn runs or `turn/start` when idle. A thread
the daemon client drives is one the hooks leave alone (`{}`); the hooks
remain for state reporting in `codex --no-daemon` and older CLIs.
Permission hooks return immediately so Codex can always show its own prompt.
Answering from the deck requires the shared app-server connection; either
client can answer, and a native answer clears the deck's card. Unrecognized
prompt types remain available in Codex. `[codex] daemon = false` turns the
client off.

Asynchronous clarification questions also appear in chat history with their
titles and suggested options. Their structured replies display as **Your
answer**, with the question above the answer, including after reconnecting.
The internal `send_user_message_question_reply` envelope stays in the Codex
transport and does not replace the task title. Quoted examples and malformed
envelopes remain ordinary text.

**Codex Desktop (macOS)** can use that same connection through its experimental
WebSocket transport over a local Unix socket. Install from the app's **Integrations → Codex Desktop** row,
or run:

```sh
deckd integrations install codex-desktop --dry-run
deckd integrations install codex-desktop
open "$HOME/Applications/Codex with OpenWalkieTalkie.app"
```

Quit Codex Desktop / ChatGPT before opening the launcher. The launcher uses the Codex CLI
bundled in `ChatGPT.app` (the renamed desktop app) or `Codex.app` to start or
reuse the shared daemon, then opens Desktop with the installer's `CODEX_HOME`
and `CODEX_APP_SERVER_WS_URL=ws+unix://localhost<resolved-socket>:/rpc`. The
socket is resolved from `$CODEX_HOME/app-server-control/app-server-control.sock`;
no TCP port is opened. Keep
deckd running with `[codex] daemon = true` (the default) and the default socket
for that same home. Desktop's local sessions then use the existing roster,
approval, question, session-control, and voice-order paths. No separate CLI
installation or hook trust is needed. Use this launcher whenever you reopen
Desktop; opening Codex normally does not opt into this mode.

App discovery checks `~/Applications` before `/Applications` and prefers
`ChatGPT.app` over `Codex.app` within each directory. This avoids launching an
older Codex installation left behind after the desktop app was renamed.

The transport was inspected in Codex Desktop **26.930.51102** (bundled CLI
**0.160.0**) and ChatGPT **26.1007.71326**. Setting only
`CODEX_APP_SERVER_USE_LOCAL_DAEMON=1` is insufficient in these builds: Desktop's
default plugin configuration adds CLI overrides, causing that branch to fall
back silently to a private stdio server. The explicit WebSocket URL selects
the shared socket directly. The shared daemon's configuration applies;
Desktop's private-server CLI overrides are not applied to the daemon.
Cloud and SSH sessions are outside this local integration.
`installed` confirms the launcher files, not that Desktop
has attached; confirm a local session appears in the roster. The launcher
refuses to interrupt a running Desktop and never restarts an existing daemon.
It also refuses to launch if the socket is missing, cyclic, or cannot be
represented safely in a WebSocket URL. Desktop's startup log should show
`hostId=local transport=websocket`; `transport=stdio` means it is still using
its private server. Launch the **Codex with OpenWalkieTalkie** application,
not the ordinary Codex/ChatGPT icon. An older OpenWalkieTalkie installer can
replace the launcher with its older template, so update the host app before
using its Install or Repair action.
`deckd integrations uninstall codex-desktop` removes the launcher while leaving
Codex, CLI hooks, and running sessions in place. Modified launcher files are
preserved and reported as conflicts.

Codex's nonblocking pending questions (`agentMessage` items with `delivery:
"async"` and `questions`) also appear on the deck. They queue one at a time,
remain available across turn completion, and return after a permission prompt
is answered. Choices and spoken replies use the question's original identity
to resolve the same entry in Codex; replies made in Codex clear the device card.
On reconnect, thread history restores unanswered questions without reviving
answered ones. History loads in small background pages, so large conversations
cannot delay live prompts with one oversized resume response. These cards do
not expire, and a failed send restores the card for another attempt.
The first successful subscription also loads history for threads announced
while deckd is running, so a short Desktop task's reply is recovered even if
the task finishes before the next roster poll.

Nothing about a CLI's prompt is ever hostage to a bug of ours. Every failure
path — an unparseable payload, a refused card, a full core, a daemon that is
not running — answers `{}` and lets the terminal dialog appear. Holding a
Claude `PermissionRequest` does **not** suppress that dialog (spike S1), so a
permission card is "first answer wins": answer it in the terminal and the
`PostToolUse`/`Stop` that follows takes it off the deck. Snooze, expiry or a
hook client that went away re-push the **same card id** as `⌨ …` with the one
choice **Open app** instead, so the drawer entry still does something useful
(it brings the session's app forward) and the deck never chimes twice for one
question.

**No deck, no collecting.** With no deck linked for ten minutes, deckd stops
collecting from agent sessions: no `claude agents --json` and no pid checks, no
transcript watchers, no Codex app-server connection, no usage reads (so no
Keychain read of the Claude login and no `curl`), and every hook is answered
`{}` with nothing behind it. The cloud voice key's Keychain item is read once,
when the service starts, deck or not, and never on a timer. The Claude and Codex sessions leave the roster
(`agent.removed{reason:"dormant"}`) and come back the way they come at start-up
the moment the deck links. Even with the deck there, the account's usage is
asked for only while the Usage face is on the strip, and the Mac's load sampled
only while the System face is. `service.dormancy` says when it stops and
starts, and `deckd status` has a `collectors:` line. `deckd run --no-device`
has no deck to wait for and collects as it always did (`docs/host-service.md`
§5.8).

**Only what the deck reads is read in full.** A session's history, tool rows,
images and plans are collected only while the deck reads it: the channel open
on it, the slot it last showed, or a card it shows, Ready cards included, and
for a minute after. Every session's state, cards and each turn's final reply
stay eager. A Claude session's transcript watcher starts when the deck opens it
and stops when the deck has moved on; a Codex thread's chat is recorded only
while it is read, and its latest items are read again when the deck opens it.
That first read replaces whatever history the deck was shown before, in order.
`--no-device` reads every session, as before (`docs/host-service.md` §5.9).

**`deckd hook <claude|codex|codex-notify|claude-statusline|gemini>`** is the
helper a CLI actually runs. It reads the event on stdin (16 MiB cap; a bigger
body is dropped, not truncated), captures the terminal environment, **walks
its own parent chain** to the first `claude`/`codex`/`gemini`/`node` ancestor
— `$PPID` is the `sh -c` that a shell-form hook runs in, and it exits a
millisecond later, so polling it would evict a healthy session within two
seconds — gives the socket 500 ms, and prints what the daemon answered.
**It always exits 0**: for both CLIs a non-zero exit from a hook is a *block*
signal, and a deckd bug must never turn into a denied command in somebody's
session. Daemon down, socket missing, timeout, malformed reply: `{}`, exit 0,
one line on stderr that names the failure and never the payload.
`claude-statusline` runs the user's previous status command with the same
stdin and copies its stdout through byte for byte; `codex-notify` takes its
payload from the last argv entry, which is where Codex puts it.

**`deckd integrations status|install|uninstall|repair <target>`** installs
the integrations. It does this itself rather than asking the daemon: the path
the hooks name is this binary's own, and no daemon has to be running.
`--dry-run` prints "What this writes" and "What this runs" and changes
nothing; the one CLI file an install still merges into (Codex's `hooks.json`)
is moved into `<data-dir>/backups/<file>.<ts>.bak` by rename first, so a
backup is inode- and mode-exact, and `uninstall` puts the original back byte
for byte.

| Target | What `install` does |
| --- | --- |
| `claude` | Writes a one-plugin **marketplace** under `<data-dir>/claude-marketplace/` (`.claude-plugin/marketplace.json`, `plugins/openwalkietalkie/.claude-plugin/plugin.json`, `plugins/openwalkietalkie/hooks/hooks.json`: exec-form `deckd hook claude` command hooks for `SessionStart`, `PermissionRequest`, `PreToolUse{AskUserQuestion}`, `UserPromptSubmit`, `Stop`, `StopFailure`, `Notification`, `SessionEnd`, and `PostToolUse` with `[hooks.claude] tool_titles`), then runs `claude plugin marketplace add <dir> --scope user` and `claude plugin install openwalkietalkie@openwalkietalkie --scope user`. Claude records the marketplace and the `enabledPlugins` flag in its own files; deckd writes **nothing** into `settings.json`. A session already open loads the plugin after `/reload-plugins`. A settings file still carrying the pre-plugin http hooks, allowlist pattern, chained `statusLine` and inventory has exactly those taken back out in the same run |
| `codex` | `$CODEX_HOME/hooks.json`: one handler group per event, appended **last**, whose `command` is a single shell-quoted string. `config.toml` is **not** written: an older install's `notify` and `tui.terminal_title` are removed when the receipt says deckd set them, and `--trust` records hook hashes under `[hooks.state]` |
| `codex-desktop` | Creates `~/Applications/Codex with OpenWalkieTalkie.app`, a launcher for Desktop's shared local-daemon mode. No edits to the Codex bundle, global environment, hooks, or trust settings. Quit Desktop and open this launcher after installation. |

What it refuses, each one measured rather than assumed:

- **A binary with no `deckd hook`.** Hooks naming it would give clap's exit 2
  and a usage error on stderr, which Codex reads as a *deny* of every
  permission request. `install` and `repair` check first and refuse.
- **A translocated path** (`/AppTranslocation/…`), because the path is
  embedded in the user's config files and disappears with the quarantine
  mount. Living outside `/Applications` is a warning, not a refusal.
- **A settings file that would be voided.** The migration that removes the
  pre-plugin entries re-reads what it wrote with Claude's own fatal-settings
  predicate (a key named `hooks`, or `PreToolUse`/`PermissionRequest`, in
  the wrong place silently voids the entire file) and restores the backup if
  it trips. The plugin itself never opens the file.
- **A Codex `command` as an array.** One of those makes codex-cli discard
  every hook in the file, on every event, with only a warning to show for it.
- **Somebody else's entries.** A foreign Codex hook group on an event we use
  is reported and appended after, never replaced; the user's own
  `settings.json` hooks, `allowedHttpHookUrls` and `notify` are not deckd's
  business any more and are not touched. A mixed hook group — one of the old
  deckd handlers beside somebody else's in the same group — is refused rather
  than dissected.

**The Codex trust step is the operator's, not ours.** A hook that deckd has
just written reports `trustStatus: "untrusted"` and **does not run** until it
is reviewed. So `install codex` ends by printing the step: open `codex`, run
`/hooks`, review deckd's entry and press `t` (on a cwd Codex has not seen,
its directory-trust prompt comes first). `deckd integrations status codex`
then reports `installed`; until then it says `untrusted`, and after a deckd
upgrade that moved the binary it says `path_drift` — the same silent stop,
for the same reason: the trust hash covers the command string, so a new path
is a hook Codex has never been shown. `modified` is the neighbouring state,
and a hash that went stale *in place* is the other way to reach it: status
asks `codex app-server`'s `hooks/list` for the trust Codex itself will act
on, so a hook Codex has quietly stopped running reads as `modified` and one
switched off in `/hooks` reads as `paused`, rather than both reading as
`installed`. When codex cannot be asked — `--no-detect`, or no `codex` on
`PATH` — status falls back to the `[hooks.state]` table in `config.toml`,
which cannot tell a current hash from a stale one; a row that fell back says
so in its own `limits` instead of passing the guess off as a check.
`deckd integrations install codex --trust` records the hashes instead,
reading them from `codex app-server`'s `hooks/list` (deckd cannot compute
them), but it is deliberately not the default: it skips a review that exists
so a user knows what runs outside their sandbox.

## Historical week-3 gaps

This table records the week-3 slice, before voice and the GUI were built;
see the design's Status section for current implementation status.
The method table is the truth — `GET /v1/schema` serves it with the scope of
every method — and it never advertises something that answers `-32601`. What
the design describes and this slice does not do, with the error each one
answers today:

| Not built | What a call gets today | Lands |
| --- | --- | --- |
| `hooks.claude_statusline` — the chained status line is installed and passes the user's own output through, but the daemon does not ingest it, so `session_name` titles and the Usage face have no feeder | `-32601 method_not_found {method, available:"week 4"}`, before the scope check; `deckd hook claude-statusline` still prints the user's line and exits 0 | 4 |
| A CLI this daemon has never heard of (`cursor`, a typo) on `hooks.<cli>` or `POST /v1/hooks/{cli}` | `-32601 method_not_found {method}` with **no** `available`: there is no release that makes it callable, and naming one would tell a client to wait for something nobody is building | — |
| The Gemini adapter: `hooks.gemini`, `POST /v1/hooks/gemini`, `deckd hook gemini`, `integrations install gemini` | `-32601 method_not_found {method, available:"v1.1"}` by name, on both doors; the installer answers "designed but not built in v1" and `integrations status gemini` reports `absent` | v1.1 |
| Voice: `voice.say\|stop\|listen\|cancel\|engines\|test` | the rate limit and the trust guard run first (so a refusal is still a refusal), then `-32013 degraded {reason:"voice_unavailable"}` | 4 |
| Desktop injection, and the Ghostty tab resolver the `SessionStart` beacon is for | `op.order{via:"inject"}` resolves to `Effect::Desktop`, which is a log line; `agent.focus`/`op.focus` do update the store and the deck, but nothing resolves or pins a terminal yet, so a `deck:` beacon goes out and nobody looks for it | 5 |
| The gpui GUI, the signed bundle | nothing to call; `tools/install-host.sh` is the interim installer | 6–7 |
| `device.raw`, `config.set`, `service.restart`, `service.stop`, `integrations.pause`, `POST /mcp` | not in the table, so `-32601 method_not_found {method}` (404 for `/mcp`). `integrations.pause` — taking the managed http entries out for a while — is a v1.1 flow; the `paused` in the status vocabulary is Codex's own `enabled: false`, which `hooks/list` reports, and is not this verb. `hooks_unreachable` and `hooks_hijack_risk` are out of the vocabulary until they have a path (the §4.1 port-degrade states, which `/v1/info.degraded` carries but `integrations.status` cannot see), and the `listener` field is out beside them. `config.get` and `config.reload` are in; `service.restart`/`stop` stay out until the daemon can ask its own runtime for an exit code | v1.1 / when the path exists |

## Build and test

Host builds always pass the host triple and their own target directory,
because the repository's `.cargo/config.toml` targets the firmware's thumb
triple:

```sh
tools/host-test.sh                 # the gate: fmt, clippy -D warnings, tests, deck-sim parity, the api and adapters steps
tools/host-test.sh --hardware      # plus `deckd run --demo` against the deck, week-1 and week-2 rows
tools/build-host.sh                # release build; prints the binary's path
```

The gate prints one line per step, the test count per crate, the ratchet
(`OWT_MIN_HOST_WS_TESTS`, raised by whoever adds tests, never lowered) and
the parity diff: `deckd device dry-run` must be byte-identical to
`tools/deck-sim.py --dry-run` for the same lines. Its `api` step then starts
a real `deckd run --foreground --no-device` on a private data dir with
`[api] port = 0` and `tcp = true` (the door is off by default, and this step
exercises it until it is deleted) and drives both doors the way a user
would: `/v1/info`
without a token, `/v1/rpc` without a token (401), `deckd token create`, a
`session.ping` with the token, a foreign `Origin` (403) and a foreign `Host`
(400), `deckd ctl session.ping` over the socket, a token minted while the
daemon runs being accepted (the `tokens.toml` watcher),
`host/examples/virtual_agent.py` answered end to end, then `SIGTERM` and a
log with no panic in it. `websocat` is used if it is installed and skipped
with a printed reason if it is not — the Rust WS test covers that door
either way. Run from inside Claude Code or Codex (a terminal one of them
started), the gate is itself an agent's process, so the service it tests
refuses it `operate`, `admin` and `device` (the ancestry cap): each row so
refused, and only for that reason, prints `skip` and says to run the gate
from a terminal of its own. The Rust tests that drive the real `deckd run`
(`no_device`, `tcp_door`, `service_exit`, `service_degrade`) assert that
refusal instead of the answer in the same case.

Its `adapters` step then exercises the installer against the CLIs that are
actually installed. It runs `deckd integrations install claude` and `install
codex` into a throwaway `CLAUDE_CONFIG_DIR`/`CODEX_HOME` under a temp `HOME`
(never `~/.claude` or `~/.codex`), asserts the second run writes nothing, and
puts what it wrote in front of the real validators: `claude doctor` on a
*copy* of the written config dir, with stdout parsed for the literal `Invalid
settings` and the exit code ignored (it is 0 even on a file Claude rejected);
the same bytes checked for the C24a landmine keys — `hooks`, `PreToolUse`,
`PermissionRequest` at any depth inside our own block — with no CLI in the
loop; and `codex app-server`'s `hooks/list` over JSON-RPC, which must return
a non-empty `hooks` array with empty `warnings`/`errors`, the only invocation
that notices an array-form `command` discarding the whole file. `install
codex --trust` then has to record a hash Codex itself handed back.
`integrations status --json` must report neither `absent` nor `conflict` for
either CLI, and the step ends by uninstalling both and diffing the three
installer-owned files (`claude/settings.json`, `codex/hooks.json`,
`codex/config.toml`) byte for byte against copies taken before the first
install, with every backup under `<data>/backups` and none beside the config.
The CLI-dependent rows — `claude doctor` on one side, `codex hooks/list` and
`codex trust` on the other — `skip` with a printed reason when that CLI is
not on `PATH`, and they are the only skips the step allows; every other row
is a pass or a FAIL.

Logs land in `target/host/gate/`. `--hardware` is not run by CI; every number it prints comes from the `status.json` the daemon writes at
shutdown or from `metrics.get`.

For one crate or one test:

```sh
H=$(rustc -vV | sed -n 's/^host: //p')
cargo test --manifest-path host/Cargo.toml --workspace --target "$H" --target-dir target/host
cargo test --manifest-path host/Cargo.toml -p deck-model --target "$H" --target-dir target/host barrier
```

The debug binary the gate builds is `target/host/$H/debug/deckd`; the release
one is `target/host-app/$H/release/deckd`.

## Running it

```sh
deckd run --foreground --demo      # the deck link plus the mockup roster, logging to stderr
deckd run --no-device              # the API only: no HID, for CI and the gate's `api` step
deckd status                       # running? sid, link, hello counts, barriers, errors, cards
deckd status --live                # the above plus `service.info` asked over the socket
deckd doctor                       # data dir, config, state, socket, tcp door, tokens, device, LaunchAgents
deckd ctl session.ping             # call one API method; the result JSON goes to stdout
deckd events --kinds 'card.*'      # one JSON line per event until Ctrl-C
deckd token create --name gate --scopes observe,agent,operate
deckd integrations status          # what is installed where, per CLI, and what is wrong with it
deckd integrations install claude --dry-run   # "What this writes", changing nothing
deckd integrations install codex   # writes the hooks, then prints the /hooks review step
deckd hook claude                  # what Claude runs; reads an event on stdin, prints the answer
deckd device metrics               # the live link metrics of the running service
deckd device flood 8000            # queue N spinner title updates (the soak)
deckd device sync                  # write a full barrier now (same sid)
deckd device dry-run --sid 314159 --hm 19:42 --flood 3 --send '{"m":"set","k":"brightness","v":2}'
```

| Command | What it does | Exit codes |
| --- | --- | --- |
| `run [--foreground] [--demo] [--no-device]` | The service. `--foreground` logs to stderr, `--demo` loads the mockup roster, `--no-device` never opens the deck. | 0 on an orderly `SIGTERM`/Ctrl-C; 0 with "already running" when the lock is held. |
| `status [--json] [--live]` | `status.json` as the daemon last wrote it, the delivery record's counts among it (its `cards since …` line; `ctl --scopes admin metrics.get` has the whole record, `docs/host-service.md` §10.6); `--live` adds `service.info` over the socket. | 0; 2 when `--live` cannot reach the daemon. |
| `ctl [--scopes a,b] [--client N] [--timeout S] <method> [json]` | One API method over the UDS door; the result JSON is printed compact on stdout, a JSON-RPC error object on stderr. | 0 ok; 1 JSON-RPC error; 2 daemon unreachable; 3 `-32002 {need:"trusted"}` (the answer has to come from the deck or the app). |
| `events [--kinds a,b] [--since REV] [--content] [--snapshot]` | One JSON line per event until Ctrl-C. `--kinds` are subscription globs (default `*`), `--since` replays the ring first, `--content` asks `observe:content`, `--snapshot` prints the snapshot first. Reconnects show as `{"type":"disconnected"}` / `{"type":"reconnected",…}` and a `{"type":"resync","rev"}` line means "call `state.get`". | 0; 2 when the daemon is unreachable at the start. |
| `token create\|list\|revoke\|rotate` | Bearer tokens for the TCP door, in `tokens.toml` (mode 0600, blake3 hashes); they open nothing while the door is off, its default, and go with it in the next release. `create` and `rotate` print the plaintext once, on stdout, and nothing else — it is never stored or logged. Edits the file directly, no daemon needed. | 0; 1 on a duplicate name, an unknown scope, a bad expiry or an unknown token. |
| `integrations status [--json] [target]` \| `install <target> [--trust] [--dry-run]` \| `uninstall <target>` \| `repair <target>` | Manages `claude`, `codex`, and the macOS `codex-desktop` launcher (`gemini` and `claude-desktop` are v1.1). No daemon has to run; `status` writes nothing at all, `--dry-run` prints "What this writes". Every replaced file is backed up by rename first. | 0; 1 on a refusal (a translocated binary, a binary with no `deckd hook`, a plan that would void the file), which names what it refused and why. |
| `hook <claude\|codex\|codex-notify\|claude-statusline\|gemini> [-- <cmd…>]` | What a CLI runs, not what a person runs: reads the event on stdin (or, for `codex-notify`, the last argument), adds the terminal environment and the CLI pid its parent walk found, and prints the daemon's answer. | **Always 0.** A non-zero exit from a hook blocks the CLI's tool call, so every failure prints `{}`, exits 0 and says one line on stderr. The `claude-statusline` passthrough is the exception that reports the user's own command's failure verbatim. |
| `device flood N \| sync \| metrics` | `ctl` calls (`admin`) against the running service. | As `ctl`. |
| `device dry-run …` | Frames the demo session offline, in `tools/deck-sim.py --dry-run`'s layout. No daemon, no HID. | 0. |
| `doctor [--json]` | Data dir, `config.toml`, `state.json`, the lock, `status.json`, the socket (path length and a `session.ping`), the TCP door (`/v1/info` is ours, not a squatter; a door that is off, by default or by `[api] tcp = false`, is said to be off, not degraded), `tokens.toml` mode, the deck, the LaunchAgents. | 0 when every check passes, 1 on any finding. |

`--data-dir` and `DECKD_DATA_DIR` are global: they move the socket, the
tokens, the port file and everything else, and every test or spike against a
real deck must use them so the user's files stay untouched.

`run` without `--foreground` logs to `~/Library/Logs/OpenWalkieTalkie/deckd.log.*`
(daily, seven kept) and opens `deckd.launchd.log` itself for anything that
reaches stdout/stderr, panics included. A second `deckd run` exits 0 with
"already running" so launchd does not loop. `Ctrl-C` or `SIGTERM` shuts the
link down in order and writes the closing `status.json`, which `deckd status`
reads back ("last known" once the daemon is gone).

`device flood|sync|metrics` are `ctl` calls over the API door like any other
(`admin`), not a private channel. Everything lives under
`~/Library/Application Support/OpenWalkieTalkie/` (`config.toml`, `tokens.toml`,
`state.json`, `status.json`, `deckd.sock`, `deckd.lock`,
`port`); `--data-dir` or `DECKD_DATA_DIR` moves all of it, which is what the
gate, the spikes and any test against the deck must use so the user's files
stay untouched. Keep that path short — the socket path has a 104-byte limit.

`config.toml` is optional and parsed in full with the design's §9 defaults.
Acted on today: `[device]` (`pace_ms`, floor 8; `hello_s`; `clock`), `[api]`
(`port`, `tcp`, `allowed_origins`, `uds_default_scope`, `uds_max_scope`,
`mailbox_ttl_s`), `[trust]` (`require_trusted_for_allow`, `gui_requirement`,
`dev_requirement`), `[log] level`, `[privacy] log_content`, `[cards]
ready_ttl_s` and the `[agents]` pins, `grace_s` and `evict`. Line
content is never logged unless `log_content = true`. The file is watched: a
change is re-read after a 300 ms debounce and applied to everything but
`[api] port`, `[api] tcp` and `[device]`, which are logged "restart needed"; a file that
does not parse or does not validate is kept out (the last good config stays
in force, the bad one is copied to `config.toml.invalid` and an `error` event
names the line). `deckd ctl config.get` prints the effective config (`api.tcp`
always a bool: the file's value, or for a key left out what `deckd run`
decided at start) and
`config.reload` forces the re-read; `tokens.toml` is reloaded by the same
watcher, so `deckd token create` works while the daemon runs.

`deckd doctor` exits 1 on any finding.

## Install and uninstall

```sh
tools/install-host.sh              # build, install, start the LaunchAgent
tools/install-host.sh --uninstall  # stop and remove it; logs and state are kept
```

The installer builds the release binary, boots any running copy out,
installs `deckd` in `~/Library/Application Support/OpenWalkieTalkie/` and the per-user
LaunchAgent `ai.cohdl.openwalkietalkie.deckd` is started with
`ProgramArguments = [deckd, run]`, `KeepAlive`, `RunAtLoad` and
`CODEX_HOME` in its environment. It runs at login; there is no tray window.
There is one HID writer per host (design D15).

To test in the foreground against the deck while the service is installed,
stop it first and restart it afterwards:

```sh
launchctl bootout "gui/$(id -u)/ai.cohdl.openwalkietalkie.deckd"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/ai.cohdl.openwalkietalkie.deckd.plist"
```

Two copies never fight: the second one sees the lock and exits.

The installer does **not** touch any CLI's configuration: the hooks are a
separate, reversible step the user takes when they want them
(`deckd integrations install claude`, `… install codex` plus its `/hooks`
review, `… uninstall <target>` to put the files back). Until then the daemon
runs with no agents but the demo's.

## Historical week-3 rollout notes

This table is the original week-3 plan, rather than current missing features.
Everything below is designed in `docs/host-service.md` and scheduled in its
§15; the weeks are the plan's, not calendar promises.

| Week | Missing today |
| --- | --- |
| 4 | Voice: capture, on-device STT, TTS with a voice per agent, the lane mixer, chimes, PTT sessions. Today only the demo reacts to a PTT release, with a scripted transcript. With it, the `statusLine` ingest: the chain is installed and passes the user's line through, but nothing reads the `session_name` or the rate limits out of it, so the Usage face has no feeder. |
| 5 | Ghostty focus, the tab resolver and the pin the `SessionStart` beacon exists for, text injection; the GUI's bootstrap. |
| 6 | The gpui `OpenWalkieTalkie.app` (Integrations, Voice, Service screens, Welcome, tray). |
| 7 | The signed bundle with `deckd` inside, SMAppService, Sparkle, notarisation, `deckd diagnostics`, `uninstall --purge`. The plist-based installer above is the interim. |
| 8 | The long soaks and the tuning of hold policies. |

Not in v1 at all (v1.1 and v2, design D18): the MCP server and Claude
Desktop, Gemini, card paging, `[scripts] on_event`, Macropad actions, the
Omarchy `ws` on macOS, `device.raw`, Linux.

## Device protocol notes

Channel 5 is the deck protocol ([docs/deck-protocol.md](../docs/deck-protocol.md)):
`[6, 5, len, payload ≤ 61, zero padding]` reports carrying newline-terminated
JSON lines of at most 1024 bytes, fragments of one line never interleaved
with another channel-5 line. Channel 2 is the device's system RPC
(`sys.version`, `sys.reboot`, `device.status`) used by the bench tools and
the daemon's USB firmware updater.

## Spikes

`host/spikes/` holds the week-1 probes (Python, not part of the build) behind
[docs/host-research/spikes-week1.md](../docs/host-research/spikes-week1.md):
the held-hook and `updatedInput.answers` runs against a real Claude Code
session, and the non-seizing HID client counts. Usage is in
`host/spikes/README.md`. Week 3's four spikes — the 140 s held request, the
`settings.json` markers, Ghostty's tab titles and the Codex trust gate — are
written up in the design's Status section, and everything they changed is in
its §5 as a "(measured 2026-09-20)" note.

## Releases and updates

[RELEASING.md](../RELEASING.md) documents the PR/main CI workflow, signed
Apple Silicon and Intel releases, automatic Sparkle app updates, and the
USB firmware updater in **Service → Updates**. Stable `vMAJOR.MINOR.PATCH`
tags publish after tests, signing, and notarization pass. Publishing requires
the GitHub `release` environment's signing secrets.

`deckd` looks for a release by itself (`firmware::schedule`): every six
hours while a deck was linked in the last ten minutes, the first time five
minutes after it starts, with the app's window closed or the app not
running. `firmware.status` then says what there is to update: the deck's
firmware (by cable, or by the version the deck named in its hello) and the
app (`app`, from the release manifest's `app.version`). `[updates] check =
false` stops the looks; `--no-device` never makes one.
