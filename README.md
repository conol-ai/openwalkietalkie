# Cohdl OpenWalkieTalkie

[Website and downloads](https://openwalkietalkie.org/) ·
[Latest release](https://github.com/conol-ai/openwalkietalkie/releases/latest)

Firmware and [routed BodyV5 r3 hardware design](hardware/rev-a/README.md) for
OpenWalkieTalkie, a handheld with a touch screen, a scroll wheel, PTT, Enter,
Esc, Shift and two front macro keys. The custom hardware includes a four-layer main PCB and
two-layer encoder PCB, with matching fabrication files and populated 3D models.

The firmware turns the device into the **Agent Command Deck**: a handheld
status board and radio for someone running several AI agent sessions at once.
The screen shows who needs you, PTT talks to the active agent, and the buttons
answer questions and switch channels.

The Rust / [embassy](https://embassy.dev) firmware currently runs on a **LilyGo
T-Display SF32** development board, which carries a SiFli **SF32LB52x** (Cortex-M33,
16MB NOR, 8MB PSRAM) and the same class of 2.16" CO5300 AMOLED with CST9220
touch. The dev board has no scroll wheel, so the **T-SF32-Keyboard** expansion
stands in for the wheel and buttons.

Sibling to [`openmicrokbd`](https://github.com/conol-ai/openmicrokbd) (v1,
STM32F072) — same approach, different silicon.

## What works today

Boot, the debug UART, the shared I2C bus, keypad and touchscreen input,
battery charging, and the Agent Command Deck's 480x480 UI on the AMOLED:
five faces, the face manager, notifications, the PTT LISTENING overlay and a
built-in demo host. Everything above the drivers is covered by host tests and
rendered previews; **none of the deck has been validated on hardware yet**
(see the [first-flash checklist](docs/deck-device-checklist.md)).

The deck is its own USB/BLE HID device: **Cohdl OpenWalkieTalkie**, VID
`0x1209` PID `0x5754`, one vendor collection (usage page `0xFF00`, report id
6, 64-byte reports). Report channel 2 carries a small system RPC
(`sys.version`, `sys.reboot`, `device.status`); the deck faces talk to a deck
host over channel 5 of the same connection ([deck protocol](#the-deck-host)).

The `1209:5754` allocation has been requested in
[pid.codes PR #1312](https://github.com/pidcodes/pidcodes.github.com/pull/1312)
and is pending registry approval. Application firmware and USB recovery use
the same product ID.

Pair **OpenWalkieTalkie** in macOS Bluetooth settings. The latest computer's bond
key and HID/battery notification preferences are stored in an append-only
flash journal at `0x12710000..0x1271FFFF`, reserved from the application
linker region. Automatic encrypted reconnection after a firmware update has
been verified. Upgrading from firmware that kept keys only in RAM requires
forgetting the old device and pairing once to create the persistent bond.

The journal checks each record's version and CRC, skips torn writes, and
verifies writes by reading them back. It supports 1024 changed records and
never erases flash; an exhausted journal reports a storage error. Unchanged
keys/preferences do not consume records. `tools/flash.sh` writes ELF load
segments and preserves the reserved journal. A whole-chip erase does not.
The Embassy target carries controls, deck traffic and battery status over HID.
On Rev A it also carries the microphone and speaker, as its own BLE GATT
service on the same link rather than as a Bluetooth audio device: a PTT press
streams 16 kHz IMA ADPCM to the host, and the host's spoken replies come back
the same way (`docs/deck-protocol.md`, "Voice"). Over the USB cable the same
frames ride report channel 7 of the HID collection, so a deck on the cable
alone has its voice too, with no pairing; `deckd` uses the cable whenever it
is there and Bluetooth otherwise. The
[native audio target](native/README.md) instead presents the display board's
microphone and speaker as Bluetooth Classic audio (HFP/A2DP) alongside the
same UI.
Both targets show the website's pixel-radio icon and stacked OpenWalkieTalkie
title on a black boot splash, with a small COHDL mark and “A COHDL PROJECT”
credit. After 1.2 seconds, the Agents face appears.
The same radio appears in Settings → About and in the Mac app, Dock, menu bar
and updater. Regenerate the desktop assets with `python3 tools/generate-branding.py`
(Pillow); normal builds use the checked-in images.

Embassy boot log, abridged:

```
SFBL
openwalkietalkie-fw 0.1.0
board: LilyGo T-Display SF32 (dev)
board: holding power rail (PA41)
board: bringing up I2C1 (SCL PA38 / SDA PA37) at 400000 Hz
display: CO5300 initialised (480x480, rotation=0deg)
board: display ready
board: ready
boot: OpenWalkieTalkie splash ready
boot: I2C1 before keyboard power-up:
  0x22 XL9555 GPIO expander (keyboard board)
  0x25 AW21009 keypad LED driver (keyboard board)
  0x34 TCA8418 keypad scanner (keyboard board)
app: Agent Command Deck ready
keypad: TCA8418 ready (5x4 matrix), PA11 interrupt + 50ms fallback
```

## Faces

The screen is a strip of faces, one on screen at a time. The default strip,
left to right: **Agents** (the agent-session roster, home), **Omarchy** (a
remote for the Omarchy/Hyprland desktop), **Macropad**, **Usage**, **System**, **Arcade** and
**Settings**.

- **Boot and home are the Agents face.** Until a deck host says hello the deck
  is empty and every face's header tag reads `offline`; a lost host leaves its
  data on screen, gray, tagged `offline`. Settings → Host → Demo data (off by
  default) fills the deck with built-in demo data instead, tagged `demo`, until
  the first real `hello` clears it.
- **SHIFT+SCROLL moves between faces**, one face per detent, clockwise to the
  right, with hard ends and no wrap. On the dev keypad: hold HOME and press ▼
  or ▲.
- **CANCEL at a face's top level goes home.**
- **Double-SHIFT** (two quick taps of HOME, nothing else in between) opens the
  face manager: reorder, add and remove faces (at most eight, three Macropads).
  Agents and Settings stay pinned.
- **PTT talks to an agent from work faces**, over a full-screen LISTENING
  overlay. On Arcade it is a local game button and never opens the microphone.
- **A new question or finished task raises a toast** for 4 seconds; tapping it
  jumps to the card. Unread notices wait in a drawer (swipe down from the top
  edge), flagged by a pill at the top of the screen, red while a question waits.
- **Idle-home:** after 120 seconds without local input the deck returns to the
  Agents roster (Settings → Display; off, 30, 60 or 120 seconds). It waits for
  a PTT session to end.
- **Sleep locks every local control, including PTT.** Any button, wheel detent
  or touch lights the lock screen. Turn the wheel counterclockwise four ticks
  to unlock; clockwise rotation resets progress. The unlocking ticks and any
  press or touch begun while locked never trigger the underlying screen.

[The Agent Command Deck](docs/agent-command-deck.md) is the full grammar: views,
cards, the manager, notifications, settings and the PTT session rules.

### Fonts

Deck text is Noto Sans (SIL Open Font License, `assets/fonts/`), rasterized
ahead of time into 4-bit glyph atlases by `tools/generate-ui-fonts.py`; the
firmware carries no font engine. The panel is small and dense (480 px over
38.8 mm, about 12 px/mm), so the type ramp runs about 1.6x the mockups' sizes:
15 px for hints, 18 px for tags and labels, 22 px body text, 25 px list rows and
up to 48 px for the big numbers (`RAMP` in `tools/generate-ui-fonts.py` and
`src/ui/theme.rs`). The mockups' layout rectangles are kept where the larger
text fits, and moved where it does not (the question card's choices start
lower; the PTT pill is wider). `tools/generate-ui-fonts.py --check`
regenerates into a temporary directory and diffs; the atlases depend on the
local FreeType build, so a check on another machine may differ.

Chinese text falls back to Noto Sans CJK SC: 28,491 glyphs covering simplified
and traditional Chinese, Extension A, Chinese punctuation and fullwidth
forms. A shared 26 px, 2-bit atlas is filtered to each text size, keeping the
firmware inside the board's application partition. The host uses the same
metrics when wrapping messages. Regenerate it with
`python3 tools/generate-cjk-font.py` (or verify with `--check`); source,
license and coverage details are in `assets/fonts/gen/README.md`.

### The deck host

The deck faces are drawn from data a host daemon pushes over HID channel 5:
agents, cards, replies, transcripts, workspaces, macros and usage. The device
sends back keys, answers, PTT edges, channel switches and settings. The wire
contract is [docs/deck-protocol.md](docs/deck-protocol.md). The host is
[`deckd`](host/README.md), the Rust service in `host/`: today it owns the link
(hello, sync barrier, heartbeat, the paced outbox), a demo roster
(`deckd run --demo`) and its open API — JSON-RPC over a Unix socket and over
`127.0.0.1:47201` (HTTP, WebSocket, SSE), so a script or a `curl` one-liner can
be an agent on the deck and answer its own cards; the Claude Code and Codex
hook adapters, voice and the gpui GUI follow the plan in
[docs/host-service.md](docs/host-service.md) (status at its top; research in
[docs/host-research/](docs/host-research/)). `tools/deck-sim.py` remains the
reference host: it runs the mockup session against a device, `--dry-run` prints
the framed reports without hardware, and `tools/host-test.sh` diffs `deckd`'s
framing against it.

Version 0.11.0 adds native companions: Conol desktop conversations, a new-chat
launcher, multi-step questions, and actual model/effort reporting on the Mac and
deck. Both apps and the firmware need compatible builds. See the
[Conol integration notes](docs/conol-integration.md) and the public
[native companion guide](https://openwalkietalkie.org/docs/native-companions/).
Other apps can use the same API; the
[standalone Python example](host/examples/native_companion.py) runs scripted
conversations and forms without Conol or a model API key.

The deck's spoken replies are bring-your-own-key: give `deckd` an OpenAI or
ElevenLabs API key (on the app's Voice screen under *Spoken replies*, or
`deckd tts key set --provider openai`, typed without echo or piped in) and
the announcements are synthesised by that provider's model
(`gpt-4o-mini-tts`, `eleven_flash_v2_5`), streamed as it is generated; without
a key, Apple's synthesiser speaks on the Mac and nothing leaves the machine.
The key takes effect at the next announcement, no restart, and a provider that
refuses or a dead network falls back to Apple's voice. Or keep the voice on the
Mac: a Kokoro voice (Chinese and English, 103 speakers, a one-time 365 MB
download that the app offers at first start, or `deckd tts install`) speaks
entirely on the machine. `deckd tts status` says which is speaking. The
Qwen3-TTS sidecar (`[voice.tts] engine = "mlx"`, `deckd tts install --mlx`,
needs `uv`) remains available for its voice quality. Speech
recognition is Apple's on-device recogniser by default, and it too can be a
local model: opt in to Whisper (large-v3-turbo, a one-time 1.6 GB download
that the app offers at first start, or `deckd stt install`) and the deck
recognises Chinese and English in one utterance, still entirely on the Mac.
Nothing is downloaded until you say so.

The Usage face is fed by `deckd` itself, the way Zeron does it: the Claude
account's rate-limit windows come from Anthropic's `/api/oauth/usage` with the
Claude Code login (on macOS the `Claude Code-credentials` Keychain item, which
macOS asks about once — *Always Allow* keeps it quiet; a *Deny* leaves the
Claude bars empty), polled every five minutes, and the Codex account's from the
Codex app-server, which also pushes them after every turn. One bar per window
(`Claude session`, `Claude week`, `Codex session`, …) with the countdown to its
reset; ENTER on the face probes both at once. `[usage] providers` in
`config.toml` picks the providers, and a chained `deckd hook claude-statusline`
is read too when the operator has one.

### System Status

The deck's **System** face shows the connected Mac's CPU load, memory used / total,
GPU load and free / total startup-volume storage. `deckd` samples every two seconds;
the meters refresh only when their displayed values change. CPU load is normalized
across all cores, memory includes app, wired and compressed memory, and storage is
the writable startup data volume. GPU load comes from the graphics driver's IOKit
counters (the busiest accelerator when there is more than one).

Unavailable counters show **Not available**. After ten seconds without a sample,
the retained readings are gray and marked **STALE**; a lost host is **OFFLINE**.
The Mac app has no additional page. System follows Usage in a fresh default strip;
for a saved layout, double-tap Shift → **+** → **System** to add it. Matching
firmware and `deckd` are required; telemetry is not sent to older decks.

### Arcade

Three offline games with original pixel art and synthesized 8-bit music:

Gameplay uses the full 480×480 display with an overlaid score. Control hints
appear when starting or pausing; Enter pauses and Cancel returns to the game list.
Snake's corner walls follow the rounded panel so food always stays visible.

| Game | Controls |
| --- | --- |
| Flappy Bird | Tap PTT to fly; avoid pipes and the top/bottom edges. |
| Snake | Turn the wheel clockwise/right or counter-clockwise/left. Left/Right buttons also turn relative to the snake's heading. |
| Raiden (雷电) | Hold Left/Right to move; the plane fires automatically. Three lives. |

Use the wheel and Enter, or touch a menu row, to choose a game. Enter starts,
pauses, resumes or retries; Cancel returns to the game list. Shift+wheel still
changes faces. Leaving the game, opening an overlay or sleeping pauses it.
Best scores last until the device restarts.

**Arcade → Sound → Enter** cycles **Off → SFX only → Music + SFX** (touch works
too). Off is the default; volume follows the Speaker setting. The three original
music loops and seven effects are synthesized locally, with no recorded audio
assets. Pausing/leaving stops sound, host speech takes priority, and microphone
capture silences game audio. A connected app remembers the sound preference;
without the app it lasts for the current boot (the next boot starts Off).

Each tune has a 48-bar arrangement with an intro, theme, answering melodies,
a quieter middle, build and return: Flappy Bird runs 96 seconds, Snake 115.2
seconds and Raiden 76.8 seconds before repeating. Bass patterns, percussion
fills and pulse tones vary between sections. Pausing or retrying the same game
continues its music position; choosing a different game starts that game's tune.

Arcade is in the new default strip before Settings. For a previously saved
layout, double-tap Shift → **+** → **Arcade** to add it. Use matching firmware and
host builds: the wire vocabulary now includes `arcade` and `arcadesound`.

Silent development checks and previews:

```sh
python3 tools/preview-all.py --board hw-a --out target/arcade-preview arcade arcade-flappy arcade-snake arcade-raiden arcade-paused
cargo run --manifest-path native/core/Cargo.toml --example arcade-audio --locked --offline --target aarch64-apple-darwin --target-dir target/native-core-preview -- target/arcade-audio
```

The second command writes three BGM WAVs and seven SFX WAVs without opening an
audio device. These are review exports; the firmware runs the same synthesizer.

## The screen

**The screen mounting is chosen per board at build time.** The default build is
the upright LilyGo T-Display SF32 development board: no physical rotation. The
custom Rev A hardware mounts the panel upside down: build it with
`--features hw-a` (`OWT_BOARD=hw-a tools/build-native.sh` for the native
target), and the boot splash, UI pixels, partial-update windows and touch
coordinates turn 180° together. The shared switch is `ROTATE_180` in
`src/display/orientation.rs`. Both targets print the rotation in their boot
log, and Settings → About shows the board and rotation, so an image built for
the wrong board is easy to spot. `tools/test-all.sh` runs every test in both
configurations. The host UI preview stays upright. See
[rotation and animation validation](docs/display-performance.md#screen-mounting-and-animation).

`hw-a` is the whole Rev A pin map, not only the rotation (`src/board.rs` has
both tables): the panel's two rails are switched on through PA11 (logic) and
PA10 (4 V) before the CO5300 reset sequence, the touch controller sits on
I2C1 at PA42/PA41 with its reset on PA44 and interrupt on PA43, the six
buttons and the wheel are read from GPIOs (`src/input/gpio.rs`), the
MP34DT05 microphone is a PDM part on PA22/PA23 with its supply switched on
PA28 (both images capture it through PDM1; `src/audio/pdm.rs` here), the
NS4150B speaker amplifier is enabled on PA20 behind the SF32's audio DAC
(`src/audio/speaker.rs`), the USB-C
data pair reaches the SF32's own USB block on PA35/PA36 so `src/link/usb.rs`
serves the deck's HID device over the cable as well as over Bluetooth
(the host prefers the cable: it syncs in well under a second and writes a
report in about 0.7 ms where the BLE link needs about 360 ms; on an
Apple-silicon MacBook the first plug-in raises macOS's "Allow accessory to
connect?" prompt and the device stays unconfigured until it is allowed; see
`docs/deck-protocol.md`), and there
is no keypad and no I2C charger: the r6 board's BQ24073 charges on its own
and firmware cannot see its state, so the battery figure follows the voltage
curve in both directions at the usual 1 %/minute, never shows charging, and
100 means the top of the curve rather than a confirmed termination. The native
image follows the same switch through
`OWT_BOARD=hw-a tools/build-native.sh`, which also builds its C side for
Rev A.

`hw-b` is the **Rev B main board** (`hardware/rev-b`): it implies `hw-a`
and overlays the three pins that moved, which is the whole difference. The
NS4150B's enable is on PA1 (PA20 is unconnected), the CAL test pad TP3 is on
PA0 (parked as a pull-down input; set SiFli's calibration tool's signal pin
to PA00), and the BQ24073's CHG output reaches PA33, so Rev B shows
*charging* while the cell takes current and confirms a full charge the way
the dev board does (the pin going high with a USB host attached and the
cell at the charge voltage; on a charger-only supply the end of the charge
reads as no input and the figure holds). Build it with `--features hw-b`
(`OWT_BOARD=hw-b tools/build-native.sh` for the native image, which defines
`OWT_BOARD_HW_B` on the C side); a `hw-a` image boots on Rev B but its
speaker stays silent. Settings → About names the board, `sys.version`
reports it, and `tools/flash-usb.py` refuses an image built for another
board than the running deck reports (`--any-board` overrides). Rev B's UART
pads are the same three, so the first flash is the same procedure, with
the pogo jig in `hardware/rev-b/jig` instead of wires. The r6 board has no USB-serial bridge: `sftool` talks to UART1 on test
pads TP6 (RX) and TP8 (TX) with TP4 as ground. A DAPLink's UART side works
there with `--compat true`, because its USB CDC bridge drops sftool's 64 KiB
stub frame. A blank board needs the flash table and bootloader at
`0x12000000` before the app: `OWT_BOARD=hw-a OWT_BOOT_IMAGES=1
tools/build-native.sh` builds both from `native/boards/openwalkietalkie/ptab.yaml`
(`build_openwalkietalkie_hcpu/ftab.bin` for `0x12000000` and
`bootloader/output/bootloader.bin` for `0x12010000`), and a chip with no
flash table waits in the boot ROM's UART download monitor, so the first
flash involves no reset timing. Later flashes need none either: the debug
port answers while the firmware runs (after a soft power-off wake the deck
by holding PTT for 2 s first), and `tools/flash-uart.py <port>
write_flash --verify <elf>` clears a stale debug session first, hands the
port to sftool and lets it soft-reset the board into the new image. The
bring-up guide has the full procedure. With the `native/bootloader` image in
the bootloader slot (see below), the application also flashes over the
board's own USB-C port: `tools/flash-usb.py <elf>`.

## Touch, battery and rendering

The CST9220 at `0x5A` uses PA0 for reset and PA1 for its active-low interrupt.
Startup checks the controller identity, then enters normal reporting mode.
Each full 15-byte report from `0xD000` is acknowledged with `0xAB`; the marker
and contact count are checked before using coordinates. The driver
exposes one contact with down, move, up, and cancellation events; touch-controller
firmware is left intact. Interrupts wake input processing immediately, with
50ms idle and 20ms active touch fallback polling. The native driver also
latches falling interrupt edges while rendering or replying to the host.
Read failures, invalid or multi-finger reports,
and 120ms without fresh reports cancel the gesture and release held controls.

**Macropad keystrokes.** A Macropad key can carry a keystroke (`cmd+shift+4`),
set in the deck's own key editor (tap `+ assign`, or SHIFT+CANCEL on the
Macropad) or by the host; `deckd` posts it to the desktop through CoreGraphics
whenever the key is pressed. The deck is deliberately not a USB or Bluetooth
keyboard: the SF32LB52x USB block has one IN endpoint besides the control
endpoint, so a keyboard interface of its own cannot exist, and a keyboard
collection beside the vendor one would make macOS class the deck as a
keyboard that no ordinary program may open (Input Monitoring), `deckd` and the
flash tool included. So the daemon types for it, which needs the
**Accessibility** permission. The first refused key press makes macOS put its
own prompt up and list the process under System Settings → Privacy &
Security → Accessibility; switch it on there and the next press goes through
(the prompt names the *responsible* process, so for a `deckd` started from a
terminal that is the terminal's app). Until then the daemon logs each refused
keystroke, `perm.status` says `accessibility: "denied"`, and `perm.request
{which:"accessibility"}` asks again.

**Battery saving.** After 30 seconds without local input the screen dims; after
2 minutes it enters panel sleep and locks all local controls, including PTT.
Any button, wheel detent or touch lights a lock screen. Four consecutive
counterclockwise wheel ticks unlock it; clockwise rotation or another sleep
resets progress. All four ticks are consumed, and a button or touch begun
while locked must be released before a fresh action can be sent. A new deck
card lights the screen for 15 seconds without unlocking or counting as input.
Held controls keep an unlocked screen awake; a button trapped down while
locked does not prevent the display from dimming and sleeping again. The lit level
follows Settings → Display brightness 1–5 (panel 40/80/130/190/255); dim is 32,
or the lit level if lower. Bluetooth audio, host status, deck messages and
charger servicing continue while dark.

**Power off (Rev A).** The board has no power switch: with a cell fitted it
runs until the firmware stops it. Hold CANCEL for 3 s (a `Power off?` dialog
with a filling bar appears after 0.7 s; letting go earlier cancels): the deck
releases every host-visible key, sends `up`, darkens and unpowers the panel
and puts the SF32LB52x into its PMU shutdown state (`src/pmu.rs`, the SDK's
`HAL_PMU_EnterShutdown` sequence through the PAC). The always-on 3.3 V rail,
the touch controller and the charger stay up, so the cell keeps charging on
USB. Holding PTT for 2 s powers it on again: the PMU wakes on PTT alone, and
the firmware goes straight back to sleep, panel still dark, if PTT is let go
before 2 s after the application starts; held through, it is a cold boot
through the bootloader with the press itself consumed. The dev board has no
such path at all. A deck that never finished the tutorial starts in it
(Settings → Display → Tutorial replays it), and Settings → About → Factory
reset blanks the pairing and the preferences and restarts.
The unused physical keypad LED rail stays off, and battery samples are taken
every 15 seconds. The native audio target also uses guarded idle clock scaling
and a 500ms dark-screen recovery poll, with immediate interrupt wake. Full system
sleep remains limited by the prototype's wake-pin wiring. See the
[battery-life plan and validation](docs/battery-life-plan.md).

**Rendering uses partial updates.** Both firmwares snapshot the UI per
frame, describe it as a signature (regions plus a hash of each region's
inputs) and redraw only what changed. The native firmware redraws the changed
regions: rows, the toast lane, the waveform or single macro keys. Each damaged rectangle fits in a 48-row band. Two 45KB buffers alternate:
the CPU draws one while LCDC DMA transfers the other at 48MHz quad-SPI. There
are no fixed waits between regions, and input/host queues are serviced between
them. A single key updates about 5% of the screen. The Embassy target redraws
the whole 48-row bands that contain a changed region, from one buffer. A face
slide redraws everything. Startup and wake redraw the entire panel before
enabling it. See [display performance](docs/display-performance.md).

The Bluetooth indicator above the header shows four RSSI bars, sampled about
every three seconds while connected. Bars are blue, or amber for a weak link;
a gray rune and dash mean disconnected, and a blue rune with unlit bars means
the reading is unavailable. USB connectivity does not change these bars.
The initial bar thresholds are −85, −70 and −55 dBm; smoothing and 3 dB
hysteresis keep small fluctuations from flickering. Signal updates do not
wake the display. Both the Embassy and native firmware use the same indicator.

The battery shows in every face's header (amber at 15% or below) and in
Settings → About. Voltage comes from the SF32's internal VBAT ADC channel 7, using factory
eFuse calibration when available. The SGM41562S at I2C address `0x03` supplies
charging status and the configured full-charge voltage. Startup verifies the
chip ID (`0x09`), sets a 350mA charging target (344mA in the chip's 8mA steps)
to 4.2V with a 500mA input limit, then enables charging. NTC temperature
monitoring, input over-voltage protection, charge termination, and an eight-hour
safety timer are enabled.
The 64-second host watchdog is fed every 15 seconds through REG0C.
Readings refresh every 15 seconds, including while the controls are idle.
While charging, the estimate is capped at 95%; 100% appears when the charger
reports completion and fresh voltage samples confirm it. Near full on USB,
charger status is checked each second to catch brief completion events; each
new completion event also triggers a fresh voltage measurement.
Percentage uses a shared stateful voltage estimator in 5% steps: USB/charger
transitions hold the previous estimate for two minutes, then progress is
limited to one internal percentage point per minute. Charging status updates
immediately at the next sample; noncharging USB cannot raise the estimate.
Confirmed full charge and sustained low voltage have separate rules.
Absolute capacity remains approximate; see [battery estimation](docs/battery-estimation.md).
This indicator does not implement low-battery
shutdown. The keypad and battery monitor share I2C1 through a blocking bus
mutex, with each transaction finishing before either task yields.

## The one idea in this codebase

`src/control/types.rs` defines what the *product* has — touch, a wheel and six buttons —
and nothing above it knows what hardware produced an event:

```rust
enum Control {
    Wheel(Wheel),           // Cw | Ccw
    Button(Button, Edge),   // Enter | Cancel | Shift | PushToTalk | Left | Right
    Touch(Touch),           // Down | Move | Up | Cancel, screen coordinates
}
```

`src/app.rs` and the whole UI are written against that. `src/input/keypad.rs`
is the only file that knows a keypad is standing in for a wheel. On the Rev A
hardware (`--features hw-a`) it is swapped for `src/input/gpio.rs`, an encoder
+ GPIO reader whose debounce and quadrature decoding live in
`src/input/direct.rs`, and nothing above `control` changes.

### Key mapping on the dev board

`src/input/keymap.rs` binds the T-SF32-Keyboard once for both firmware targets.
Up, dot and down run down the left edge, so the dot reads naturally as ENTER
between the two wheel keys; the top row carries CANCEL, SHIFT and PTT, and `1` /
`2` beside the dot stand in for the front LEFT / RIGHT keys.

| Keypad key      | TCA8418 key | Control                               |
|-----------------|-------------|---------------------------------------|
| ▼ down          | 21          | SCROLL clockwise ("next")             |
| ▲ up            | 41          | SCROLL counter-clockwise ("previous") |
| ● dot           | 31          | ENTER                                 |
| ESC             | 42          | CANCEL                                |
| HOME            | 43          | SHIFT (modifier only)                 |
| ✉ envelope      | 44          | PTT (held)                            |
| `1`             | 32          | LEFT                                  |
| `2`             | 33          | RIGHT                                 |

Chords on the dev keypad: HOME + ▼/▲ = SHIFT+SCROLL (next/previous face),
HOME + ESC = SHIFT+CANCEL, HOME HOME = double-SHIFT (face manager).

Holding up or down auto-repeats after 400ms at 90ms intervals, so a held key
feels like a spun wheel; the repeat stops after 10 seconds in case a release
was lost. Every other key, including the old ✕ cross (11), is unbound. Unbound
keys are logged with their number and matrix position, which is how you read
off a code when re-mapping. If the keypad's I2C bus fails, every button is
released, once per outage, so nothing stays held on the host.

## Layout

| Path | Contents |
|---|---|
| `src/board.rs` | Pin maps and bring-up for both boards (LilyGo dev board by default, Rev A with `hw-a`). Logging comes up first so the rest can narrate itself. |
| `src/control.rs` | The product's input vocabulary and the event channel. |
| `src/control/types.rs` | Hardware-independent wheel, button, and touch events. |
| `src/app.rs` | Embassy application loop: controls, deck lines and link status into `UiState`, power policy, banded partial redraw. |
| `src/power.rs` | Dim/off policy, dark-screen wake rules, notice wake, brightness levels; shared by both runtimes. |
| `src/link/` | HID transport over BLE and (Rev A) USB: the system RPC on channel 2, deck lines on channel 5, one ordered outbound queue, bond journal. |
| `src/deck/` | Agent Command Deck data: `Text<N>`, the channel-5 message vocabulary (`msg.rs`), the model (`model.rs`), the no-alloc JSON reader/writer and line assembler (`json.rs`, `proto.rs`), and the demo host (`demo.rs`). |
| `src/ui/mod.rs` | Shared UI root, mounted as `ui` by both firmware targets, the host tests and the preview. |
| `src/ui/state/` | `UiState`: the face strip, overlays and per-face grammars; `mod.rs` is the dispatch skeleton, `global.rs` the SHIFT/PTT/teardown layer, `touch.rs` the touch recogniser, one file per face beside them. |
| `src/ui/scene.rs` | Frame dispatcher: one or two faces of the strip at their x origin, then the overlays; assembles the frame signature the same way. |
| `src/ui/faces/` | One renderer per face template: `agents`, `omarchy`, `macropad`, `usage`, `settings`. |
| `src/ui/overlay/` | LISTENING (`ptt.rs`), toast/drawer/indicator (`notify.rs`), manager/gallery/naming/confirm (`manager.rs`). |
| `src/ui/signature.rs` | Frame signature: mode plus hashed regions. |
| `src/ui/damage.rs` | Changed rectangles between two signatures, one per 48-row band. |
| `src/ui/theme.rs`, `src/ui/layout.rs` | Palette and type ramp; integer rectangles of every hit and draw target, measured from the mockups. |
| `src/ui/kit.rs`, `src/ui/font.rs`, `src/ui/font_data.rs` | Shared widgets (header, hint bar, keycaps, cards, lists); the atlas text blitter and its generated metrics. |
| `assets/fonts/` | Noto Sans TTFs, OFL, and the generated glyph atlases in `gen/`. |
| `docs/deck-protocol.md`, `tools/deck-sim.py` | The channel-5 host protocol, and a hidapi host simulator that runs the mockup session against a device. |
| `tools/generate-ui-fonts.py` | Rebuild the glyph atlases and `font_data.rs` from the TTFs (`--check` diffs). |
| `tools/preview-all.py` | Render every `ui-preview` scene to PNG and lay the mockup scenes over the mockups. |
| `assets/lucide/` | Official Lucide glyph subset, generated alpha masks, provenance and license. |
| `host/` | The `deckd` workspace: the macOS host service that owns the deck link over HID (channel 5) and serves the open API (Unix socket and `127.0.0.1:47201`); the adapters, voice and GUI are still to come. |
| `tools/host-test.sh` | The host gate: fmt, clippy, every workspace test, `deck-sim.py` parity, and an `api` step that drives a real daemon through both API doors; `--hardware` adds the device checks. |
| `tools/install-host.sh` | Build, install, start, or uninstall the per-user `deckd` LaunchAgent. |
| `tools/generate-ui-glyphs.py` | Rebuild the icon masks from the vendored font using Pillow. |
| `src/input/cst9220.rs` | Controller identification and report/acknowledge protocol. |
| `src/input/touch.rs` | Reset, GPIO interrupt handling, and touch event delivery. |
| `src/input/touch_state.rs` | Contact lifecycle and invalid/stale contact cancellation. |
| `src/display.rs` | CO5300 panel over the LCDC in quad-SPI, plus the band framebuffer. |
| `src/display/orientation.rs` | The per-board 180° switch, shared by display and touch. |
| `src/input/keymap.rs` | Dev-keypad key numbers → wheel and buttons, shared by both firmware targets. |
| `src/input/keypad.rs` | Dev-board shim: keypad → `Control`. The one file the real hardware replaces (default build only). |
| `src/input/gpio.rs` | Rev A (`hw-a`): the six buttons and the wheel on GPIOs → `Control`; sleeps on any edge, samples fast while a contact settles. |
| `src/input/direct.rs` | Rev A input decoding without hardware: per-button debounce and the encoder's quadrature cycle (one detent = one `Wheel`), shared with the native runtime and host-tested. |
| `src/input/tca8418.rs` | TI TCA8418 keypad scanner driver. |
| `src/input/xl9555.rs` | XL9555 GPIO expander — gates the keypad's reset and rails. |
| `src/logger.rs` | Line logging over UART1. |
| `src/battery/mod.rs` | Periodic ADC sampling, charger status, and watchdog service. |
| `src/battery/charger.rs` | SGM41562S configuration, readback verification, and transaction tests. |
| `src/battery/state.rs` | Calibration, voltage estimates, and host-runnable tests. |
| `tests/host/` | Host test crate: protocol, grammar, demo and touch integration tests plus every shared file's inline tests. |
| `native/` | RT-Thread firmware with Bluetooth audio; its Rust core mounts the same `ui` and `deck`. See [native/README.md](native/README.md). |
| `tools/flash.sh` | Flash via sftool, then tail the log. Used as the cargo runner. |
| `tools/test-all.sh` | Every test crate and firmware build for both boards, with test counts. |
| `tools/check-stack-frames.py` | Reads function prologues out of an ELF and fails on an oversized frame of ours. |
| `tools/monitor.py` | Serial monitor (1 Mbaud needs an ioctl; see below). |

## Building and flashing

Needs the `thumbv8m.main-none-eabihf` target and
[`sftool`](https://github.com/OpenSiFli/sftool):

```sh
rustup target add thumbv8m.main-none-eabihf
cargo install sftool
```

Then, with the board on USB:

```sh
cargo run --release                    # LilyGo board: flash, then tail the log
cargo run --release --features hw-a    # Rev A hardware (r6 pin map, panel turned 180°)
cargo run --release --features hw-b    # Rev B hardware (three pins moved; implies hw-a)
tools/monitor.py                       # just tail the log
```

`probe-rs` is not used to flash: on the SF32LB52 it can only `attach`, never
`download`. `sftool` takes the ELF directly, so there is no objcopy step.

Battery, charger, touch protocol, deck protocol and UI grammar tests run on the
host without flashing:

```sh
cargo test --manifest-path tests/host/Cargo.toml --lib \
  --target "$(rustc -vV | sed -n 's/^host: //p')" \
  --target-dir target/host-tests --locked --offline
```

Add `--features hw-a` (or `hw-b`) for the rotated build and a test-name
filter after `--offline` to run a subset.

`tools/test-all.sh` is the complete gate: it runs these and the native core
tests (render and fuzz tests included) for every board (default LilyGo,
`--features hw-a` and `--features hw-b`), builds the Embassy firmware and the
native staticlib each way, runs the C checks, and prints the passed-test
count of every step. Test
counts are ratcheted (`OWT_MIN_HOST_TESTS`, `OWT_MIN_NATIVE_TESTS`): a change may
add tests, never lose them. `OWT_GOLDEN_DIR=<dir>` also compares
`ui-preview` renders byte for byte with saved PPMs, and `--native-link` (with
`OWT_SDK_ENV` naming the SDK environment script) links the native firmware for
every board and runs `tools/check-stack-frames.py` over both firmwares: no Rust
function of the native image, and nothing in the Embassy main task, `app`, `ui`
or `deck`, may reserve more than 2 KB of stack (the UI state is filled in
place, never moved).

```sh
tools/test-all.sh
OWT_GOLDEN_DIR=/path/to/golden tools/test-all.sh
OWT_SDK_ENV=/path/to/soc-build-env.sh tools/test-all.sh --native-link
python3 tools/preview-all.py --out target/preview
```

`tools/preview-all.py` renders every scene of the native core's `ui-preview`
example to PNG (every deck face and view,
overlays, toasts and slide frames) and writes side-by-side sheets of the
scenes that have a mockup in `docs/mockups/`. Name scenes to render a subset;
`--board hw-a` (or `hw-b`) builds the preview with that board's feature.

**Only the application partition at `0x12020000` is ever written.** The flash
table and bootloader are left alone, so a bad image is always recoverable by
flashing a good one — no JTAG rescue needed. `memory.x` records the partition
map, read back off the board itself with
`sftool read_flash ftab.bin@0x12000000:256`.

**Rev A executes from PSRAM to suppress flash-coupled speaker hiss.**
`hw-a` without `hw-b` selects `link-psram.x`: Reset and the startup copy
routine stay in NOR, while `.text`, `.rodata` and `.flash_assets` (including
the Chinese fonts) run/read from PSRAM. `src/xip.rs` copies and verifies
these regions and moves the interrupt vector table to SRAM. Three failed
copies return to USB recovery. Rev B and LilyGo use cortex-m-rt's `link.x`
and execute directly from NOR. On every board, normal `.data`
initialization copies mutable data and the small flash-writing/shutdown
routines into SRAM. PSRAM's final MiB is reserved for bench diagnostics;
Rev A's copies and runtime buffers must fit in its first 7 MiB.

The 16 MiB NOR now provides **15.75 MiB for application code and assets**.
The lower bank (`0x12020000..0x126FFFFF`, 6.875 MiB) holds code, ordinary
constants and SRAM initialization data. `flash-assets.x` places the Chinese
atlas and metrics in the upper bank (`0x12720000..0x12FFFFFF`, 8.875 MiB).
Rev A's `flash-assets-psram.x` keeps their load image at the same address.
The two intervening 64 KiB Bluetooth stores retain their original addresses;
the initial 128 KiB retains the flash table and bootloader. New large assets
can use `.flash_assets.*` sections without taking space from the lower bank.

Firmware 0.5.0 supports these U4 flash choices on both board revisions:

| LCSC part | Device | JEDEC ID (wire order) |
| --- | --- | --- |
| C97521 | Winbond W25Q128JVSIQ | `EF 40 18` |
| C5354566 | Puya PY25Q128HA-SUH-IR | `85 20 18` |
| C2928891 | GigaDevice GD25Q128ESIGR | `C8 40 18` |

All are 3 V, 16 MiB parts and use the same partition map. Recovery's pinned
SDK already recognizes all three; the application's settings/bond journal
and temporary bootloader updater use the same explicit ID allowlist and
reject unstable or unknown IDs before enabling writes. Existing program and
erase timeouts cover the specified industrial parts. Flash clock and quad-read
configuration remain under the SDK's control. The alternate parts still need
physical cold-boot, XIP, settings/bond persistence and USB recovery/migration
qualification on populated boards; software checks alone do not qualify a
production substitution. See the [Puya datasheet](https://www.puyasemi.com/download_path/%E6%95%B0%E6%8D%AE%E6%89%8B%E5%86%8C/Flash/PY25Q128HA_Datasheet_V2.3.pdf)
and [GigaDevice datasheet](https://download.gigadevice.com/Datasheet/DS-00480-GD25Q128E-Rev1.4.pdf).

**Existing Rev A/B devices need the protocol-2 recovery bootloader before an
image using upper flash can be installed over USB.** The desktop updater can
perform this migration using a temporary lower-bank app and the board's recovery
image after explicit acceptance of the bootloader power-loss warning. See
[the update sequence and Rev B hardware validation gate](RELEASING.md#device-firmware-updates).
UART also supports blank boards and incompatible flash-table updates.
The USB tools and desktop updater reject an incompatible recovery version
before erasing any application data. Updated recovery still accepts older
small images. Raw release binaries contain `0xFF` padding over the bond
stores, which the USB updater skips; **use the ELF for UART flashing**, never
write the padded binary as one contiguous range. File size includes padding
and is not the amount of code/assets stored.

**Rev A recovers over its own USB-C.** `OWT_BOARD=hw-a OWT_BOOT_IMAGES=1
tools/build-native.sh` builds `native/bootloader`, the SDK's second-stage
bootloader with a USB recovery mode (details in
[native/README.md](native/README.md)). It checks the application's vector
table before every boot and enters recovery when the image is invalid, when
SHIFT and CANCEL are held while the deck powers on, or when the running
application is asked to (`sys.reboot` with `mode: recovery`). Recovery mode
is the deck's own USB device, descriptor for descriptor (the
"OpenWalkieTalkie" vendor HID collection), so no driver and no new-accessory prompt are
involved; the image travels on channel 9 of the same reports.
`tools/flash-usb.py <elf>` drives the whole cycle from a running deck:
request, download (about 32 s for a 1 MB image), and the wait for the
application to come back; `--no-request` when the deck is already in
recovery. Both boards build to the same ELF path, so the tool checks the
image's Rev A/Rev B identity against the connected board and rejects
unidentified dev-board images. Current XIP and older PSRAM-linked images
are supported without a special flag. Build the intended board last.
The desktop migration can replace a compatible device's bootloader over USB.
Writing the flash table or provisioning a blank board still requires
the UART pads (`tools/flash-uart.py <port> write_flash --verify
native/project/build_openwalkietalkie_hcpu/bootloader/output/bootloader.bin@0x12010000`),
which the chip's debug port answers whatever is in flash.

## Things that cost time, written down

- **`sifli-hal` is pinned to a git revision, not the crates.io release.**
  `sifli-hal 0.1.1` on crates.io has no I2C driver at all, and I2C is the entire
  input path here. The HAL is pre-1.0 and explicitly "working in progress";
  re-pin deliberately.
- **UART1 is PA18 = RX, PA19 = TX** on this board — the reverse of the SF32LB52
  devkit that upstream's examples target. `Uart::new_blocking` takes
  `(peri, rx, tx)`, which happens to make the same argument order correct for
  both, for opposite reasons.
- **Do not blind-scan the I2C bus.** Probing address `0x5A` wedges the HAL's
  blocking I2C driver permanently, with no timeout. A boot-time sweep therefore
  hangs the firmware at `0x5A` having logged everything below it, which reads
  exactly like a crash somewhere else entirely. `input::probe_known` probes only
  the addresses this firmware depends on.
- **Upstream's CO5300 example does not compile,** so this does not use it. It
  imports `display_driver::bus::QspiFlashBus`, which is absent from the
  `display-driver 0.1.1` its own lockfile pins. `src/display.rs` drives the
  panel through `sifli-hal`'s LCDC directly instead, using the init sequence
  LilyGo ships. The LCDC frames quad-SPI itself; we send the panel's DCS
  commands and set its address window.
- **Compose the quad-SPI header yourself; the LCDC does not.** This panel has
  no D/C pin, so a transaction opens with a one-byte opcode and a 24-bit
  address carrying the command: `[0x02, 0x00, cmd, 0x00]` to write a register,
  `0x32` in place of `0x02` for pixels, `0x03` to read. The word goes out four
  bytes wide. Sending a bare command byte is accepted by the controller and
  understood by nothing -- transfers complete, every call returns `Ok`, and the
  panel stays asleep and black. Nothing in the log distinguishes this from a
  working display, which is what makes it expensive.
- **`send_pixel_data` sets the *controller's* ROI, not the panel's.** Both have
  to be told about a partial update or the pixels land somewhere else entirely,
  which is why every band write is preceded by `0x2A`/`0x2B`. It does not send
  the memory-write command either -- `0x32`/`0x2C` goes out first, with
  chip-select held so the DMA continues the same transaction.
- **The panel's reset is PA9, not PA0.** PA0 is the *touch* controller's reset
  on this board; the upstream example's PA0 would leave the panel held in reset
  and the screen black with no error anywhere.
- **The command page unlock is `0xFE`/`0x00` here**, not the `0x20` upstream
  uses for the SiFli reference panel. Get it wrong and every later command
  writes into the wrong register page.
- **The band buffer is `repr(align(4))` and lives in `.bss`.** The LCDC DMAs
  straight out of it and asserts 4-byte alignment, and at 45KB it is far too
  large to build on the stack on its way into a `StaticCell`.
- **Never move a `UiState` by value.** It is several kilobytes (so are `Deck`,
  `HostMsg` and `Signature`), and the native application thread's stack was
  12 KB: one `let state = UiState::new()` or a by-value return overflows it with
  no useful fault. Both firmwares keep these in statics, fill them in place
  through `&mut` and snapshot with `copy_into`. The native main thread now has
  24 KB (`native/project/proj.conf`), and `tools/check-stack-frames.py` fails
  the link gate on any function of ours reserving more than 2 KB, so a
  regression shows up as a test failure rather than a HardFault on the desk.
- **Every pixel a face draws must be in a signature region that hashes its
  inputs.** Partial redraw only repaints regions whose hash changed; anything
  drawn from state that no region hashes stays stale on the panel while every
  single-frame test passes. Anything displayed from time (ages, the waveform,
  the toast slide) is recomputed in `tick` and reported dirty. The fuzz walk in
  `native/core/src/render_tests/fuzz.rs` drives random input through the UI and
  asserts partial == full after every step, in both orientations; run it after
  touching any renderer or signature.
- **Channel 5 is a line stream, not channel 2's message framing.** The
  system-RPC reassembler (`wire::Reassembler`) takes one JSON request at a time and
  clears after it; deck lines are LF-terminated, several may share a report and
  one may straddle many. Feeding channel 5 through the channel-2 path loses the
  second line of every shared report. `deck::proto::LineAssembler` splits on LF
  only and discards an overlong line up to its LF.
- **The keypad is behind the XL9555.** The TCA8418 is held in reset until the
  expander drives P0_7 high, so it does not answer on the bus until the expander
  has been configured. Rails first, then release reset.
- **PA41 is `3V3_EN`** and must be driven high and left there. The `Output` is
  deliberately leaked; dropping it would cut power to the board mid-run.
- **PA44 enables the display supply.** LilyGo's `BSP_LCD_PowerUp` drives this
  pin high as `LCD_VADD_EN`; holding PA41 high is not enough. The display owns
  the PA44 output and lets the supply settle before its reset sequence. LCDC
  transfers completing do not prove the panel is powered or showing pixels.
- **The pinned HAL's ADC needs explicit slot routing.** Its VBAT read enables
  slot 7 and reads RDATA0 without setting the slots' input selectors. Route
  slots 0 and 7 to channel 7 before sampling; otherwise this board reports
  about 1330 counts from the wrong input. Scale conversion timings to PCLK as
  in SiFli's `HAL_ADC_SetFreq`, and use the primary 3.3V factory calibration
  points plus the VBAT reference point to account for offset and divider gain.
- **Identify the charger before programming it.** This board reports ID `0x09`,
  the SGM41562S, despite LilyGo's example driver being named `sgm41562b`.
  The S variant uses REG04 bits 7:1 with 10mV voltage steps; REG0C holds the
  input current limit and watchdog reset. Copying the B driver's watchdog
  write would instead raise charging current. Unknown IDs are left unconfigured.
- **Keep BLE controller sleep disabled with the pinned radio stack.** With
  `pm_enabled(true)`, macOS discovers the device but connections repeatedly
  end in `0x3E` (failed to establish) or `0x08` (timeout), before pairing.
  Disabling sleep permits encrypted pairing and system RPC requests.
  Starting the board's Y6 crystal on PA22/PA23 and observing `LXT_CR.RDY`
  did not resolve the sleep failure. The workaround increases battery use;
  the radio's sleep timing/clock path needs investigation before re-enabling it.
- **macOS `stty` cannot set 1000000 baud** — it fails with `tcsetattr: Invalid
  argument`. The rate has to be set with the `IOSSIOSPEED` ioctl *after* the
  port is open, which is why `tools/monitor.py` exists instead of `stty` + `cat`.
- **macOS asserts RTS on port open, and RTS is wired to reset.** Attaching a
  terminal without clearing it holds the board in reset and the port stays
  silent — indistinguishable from firmware that never booted.
- **Never leave a monitor attached while flashing.** There is one serial port,
  and it carries both the log and the bootloader handshake. A monitor running in
  another window eats the bytes sftool is waiting for, and every attempt fails
  with `Failed to connect to the chip` or `timeout while waiting for RAM command
  response`. This looks exactly like a wedged board, and it is not one --
  `tools/flash.sh` warns if something else holds the port. (The firmware
  logging on that same UART adds some genuine flakiness on top, which is why
  the script also retries; upstream's UART examples carry the same warning.)
- **Do not round-trip the port's baud rate.** After the monitor has used
  `IOSSIOSPEED`, the port sits at a custom 1000000 that `sftool`'s serial layer
  rejects outright with `Invalid argument`. `stty -f <port> 115200` puts it
  back.

## Hardware references

The [BodyV5 r3 hardware release](hardware/rev-a/README.md) contains the routed
boards, matching CoHDL source, fabrication/assembly files and populated STEP
models. It uses a four-pin 0.5 mm inter-board FPC. Its GPIO map is separate
from the LilyGO firmware target; physical fit checks and SGM41562SA charger
firmware support remain as documented in the release notes.

Use the [hardware design constraints](docs/hardware-design-constraints.md) when
designing our PCB, especially wake-pin allocation and independent input/display
power domains. Verified prototype limitations and proposed requirements are
recorded separately there.

Pin map and driver sequences are transcribed from LilyGo's board definition and
their SDK fork, [`Xinyuan-LilyGO/SlFli-SDK-Lilygo`](https://github.com/Xinyuan-LilyGO/SlFli-SDK-Lilygo)
(`customer/boards/t-display-sf32-base/bsp_pinmux.c` for pins,
`t_sf32_display_hw_device/` for the keypad, expander and LED drivers). The
TCA8418 key numbering is TI's: `row * 10 + col + 1`, in decimal, so the tens
digit is the row.

Unlike `openmicrokbd` v1, whose pin map is generated from CoHDL source, these
are a vendor board's and are transcribed by hand — the boot-time I2C probe is
what confirms them.

## License

The software is MIT — see [LICENSE](LICENSE). That is everything outside
`hardware/` (firmware, native image, host crates, tools, packaging) plus the
scripts inside `hardware/`, which carry `SPDX-License-Identifier: MIT` headers.

The hardware and mechanical design is CERN-OHL-P-2.0 (the permissive CERN
Open Hardware Licence) — see [hardware/LICENSE](hardware/LICENSE) and
[hardware/README.md](hardware/README.md) for what that covers (CoHDL and KiCad
sources, fabrication outputs, BOMs, case and jig CAD).

Noto Sans is under the SIL Open Font License 1.1 (`assets/fonts/OFL.txt`); the
Lucide glyphs keep their own license in `assets/lucide/`; third-party CAD and
KiCad models under `hardware/` keep theirs (see the `ATTRIBUTION.md` files).
