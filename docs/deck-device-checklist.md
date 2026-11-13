# Agent Command Deck: first-flash checklist

Nothing in the deck has run on hardware yet. Work through this on the LilyGo
T-Display SF32 with the T-SF32-Keyboard, first with the native image (the
daily firmware, with audio), then a shorter pass with the Embassy image, whose
redraw path differs (whole bands, one buffer). Write the date, image and result
of each section into [native/README.md](../native/README.md) or
[display-performance.md](display-performance.md), as the existing validation
notes do. The expected behaviour of every step is in
[agent-command-deck.md](agent-command-deck.md).

Keypad, for every step below:

| Key (TCA8418) | Control | Key (TCA8418) | Control |
|---|---|---|---|
| ▼ down (21) | SCROLL CW | HOME (43) | SHIFT |
| ▲ up (41) | SCROLL CCW | ✉ envelope (44) | PTT |
| ● dot (31) | ENTER | `1` (32) | LEFT |
| ESC (42) | CANCEL | `2` (33) | RIGHT |

HOME + ▼/▲ = next/previous face, HOME HOME = face manager.

## 0. Before flashing

- [ ] `tools/test-all.sh` is green, and `OWT_SDK_ENV=... tools/test-all.sh
      --native-link` links both boards.
- [ ] Native: `tools/build-native.sh` prints `board=lilygo (rotation 0 degrees)`;
      `python3 tools/check-native-image.py native/project/build_openwalkietalkie_hcpu/main.elf`
      passes; flash with `tools/flash.sh` (no monitor attached).
- [ ] Embassy: `cargo run --release` (default features = LilyGo).

## 1. Boot and orientation

- [ ] Log shows `board: lilygo image, rotation=0deg` (native) or
      `display: CO5300 initialised (480x480, rotation=0deg)` (Embassy), then
      `UI: first frame ready` / `app: Agent Command Deck ready`.
- [ ] The splash is upright with the board held normally, for about 1.2 s,
      then the Agents roster with the `demo` tag top right.
- [ ] Settings → About → Board reads `LilyGo · 0°`.
- [ ] Touch corners land where touched: on Settings the `Volume` tab (top
      left) and `About` tab (top right); on Macropad the top-left and
      bottom-right keys.
- [ ] A swipe down from the physical top edge opens the drawer; the same swipe
      from the bottom does nothing.
- [ ] `display: full frame=...` and the first `display: partial ...` lines
      appear in the native log; record them (deck-face render cost is
      unmeasured, see [display performance](display-performance.md#agent-command-deck-faces)).

## 2. Faces, one by one (demo data)

Agents:
- [ ] ▼/▲ move the highlight over five agents ("1 more ↓" at the bottom);
      `1`/`2` move it and the active-channel bar follows.
- [ ] ● on Codex · cohdl-api opens its channel; ● again opens the question
      card; ▼ changes the choice; ● confirms (not within 0.5 s of the card
      appearing). The card shows "sent", then clears, and the agent turns
      Working (blue).
- [ ] About 20 s later (screen on) a new question raises a toast; tapping it
      jumps to the card.
- [ ] On Claude · web-ui's Ready card, ● with no step highlighted acknowledges
      it and opens the channel; ESC returns to the roster.
- [ ] Tap a row, then tap it again: highlight, then open.

Omarchy, Macropad, Usage, Settings:
- [ ] Omarchy: `1`/`2`/● change the demo workspaces; ▼/▲ move the window
      highlight; tapping a chip or window row reacts; ESC goes home.
- [ ] Macropad: `1` = build, `2` = test, ● = deploy, HOME+`1` = git push,
      HOME+`2` = mute, HOME+● = term show pressed while held; the dashed
      screenshot/lock keys press only by touch; ESC goes home.
- [ ] Usage: ▼/▲ scroll; ● shows "updating…" for 1.5 s and the bars move.
- [ ] Settings: `1`/`2` and tab taps switch panels, ● moves rows, ▼/▲ adjust.
      Brightness 1–5 visibly changes the panel at once. Demo data off empties
      the roster; on restores it.

## 3. Slide

- [ ] HOME+▼ four times walks Agents → Omarchy → Macropad → Usage → Settings, about 220 ms per slide; one more gives a small bounce, no wrap.
      HOME+▲ walks back.
- [ ] Hold HOME and hold ▼ (key repeat): at most one face per 150 ms, and no
      stray scroll on the face after HOME comes up.
- [ ] Watch each slide for tearing, stale strips at band edges, text shimmer
      and a slow final frame. Note anything uneven per face pair.

## 4. Face manager

- [ ] HOME HOME opens it on the current face; HOME HOME again (or a tap on a
      card) exits to the selected face. A third key between the taps cancels
      the double-tap.
- [ ] ● grabs Macropad, ▼ moves it, ● drops it; the strip order changes.
      A 350 ms long-press grabs by touch and the card follows the finger.
- [ ] ESC on Usage asks to remove it; ● removes, ESC keeps.
- [ ] "+" → gallery → Macropad → naming: hold ✉ and release, the name
      "Deploy" appears; ● adds the face before Settings.

## 5. Notices

- [ ] A toast slides down for 4 s, then the indicator pill appears (red while
      a question waits).
- [ ] Swipe down from the top edge opens the drawer: ▼ selects, ● jumps to the
      card, `1` or a sideways swipe dismisses, ESC closes.
- [ ] While the drawer or manager is open a new notice goes straight to the
      drawer, with no toast.

## 6. PTT

- [ ] On the roster, hold ✉: the LISTENING overlay shows the highlighted agent
      and a moving waveform, the demo transcript types out; release ends it
      and the reply appears in the channel.
- [ ] Hold ✉ on a question card: "Answering · …" with the question.
- [ ] Hold ✉ then press ESC: the session aborts. Hold ✉ past 60 s: it ends by
      itself.
- [ ] During a session HOME+▼ and HOME HOME do nothing.

## 7. Dark screen and idle-home

- [ ] Dim at 30 s, off at 2 min (`power: screen off` in the log). Leave the
      deck on Omarchy: after 2 min it wakes on the Agents roster.
- [ ] While off, ● only wakes: nothing opens on the press or the release. Same
      for ▼ (the highlight does not move), ESC, HOME, `1`, `2` and a touch.
- [ ] While off, hold ✉: the screen wakes and LISTENING starts at once.
- [ ] While off with deck-sim running (section 9), a new card wakes the screen
      for 15 s and it goes dark again. The demo's own notices never wake it.
- [ ] Settings brightness 1: dim level is not brighter than lit.
- [ ] Rev A: hold ESC on the roster. `Power off?` with an amber bar appears
      after 0.7 s; release at 2 s closes it and nothing else happens. Hold it
      through 3 s: the panel goes dark, the log shows `power: off`, `power:
      panel rails off` and `power: entering shutdown`, and the Mac loses the
      USB device. Only PTT powers it on, held: a short press logs `boot: woke
      from shutdown by PTT` then `power: PTT released … back to sleep` and
      the panel never lights; held through, `power: PTT held through the
      2000 ms power-on hold` and the splash follow. ESC, ENTER, SHIFT, `1`,
      `2` and the wheel do nothing while off. The PTT that woke it does
      nothing else.
- [ ] Rev A: hold ESC on a Question card: `snooze` goes out first, then the
      dialog. Hold ESC while off: it only wakes. On Omarchy SHIFT+ESC goes to
      the host and never powers off. Settings → About
      shows `Power off · hold CANCEL 3 s`.
- [ ] Rev A, first boot after a factory reset (or with a blank journal): the
      tutorial opens after the splash. It asks for one key at a time, lit on
      the drawing of the deck where the real key is (ENTER top right, PTT
      left edge, wheel top left); a wrong key is named in amber and nothing
      moves; the right one goes on to the next. The wheel step counts down
      three detents; SHIFT+SCROLL moves
      the strip chip (the face beneath never slides); SHIFT+ENTER teaches
      returning to the latest chat message and resuming live following,
      with both keys lit (ENTER alone leaves this step open); PTT shows LISTENING
      with no `ptt` line to the host; ENTER at 6 / 6 closes it and the log
      shows `link: preferences saved to flash`. The next boot goes straight
      to the roster.
- [ ] Settings → Display → Tutorial replays it; CANCEL on step 2 returns to
      Settings. Holding ESC 3 s inside the tutorial still powers off.
- [ ] Settings → About → Factory reset shows `Factory reset?`; ESC keeps;
      ENTER logs `app: factory reset confirmed` and `link: storage erased`,
      the deck restarts unpaired (forget it in macOS Bluetooth settings and
      pair again) and in the tutorial.

## 8. Identity and system RPC

- [ ] macOS Bluetooth settings list **OpenWalkieTalkie** (a deck once paired
      as **Codex Micro** must be forgotten and paired again); System
      Information → USB (Rev A, cable) shows `OpenWalkieTalkie`, vendor
      `0x1209`, product `0x5754`, manufacturer `Cohdl`. The first plug-in on a
      Mac asks to allow the new accessory once.
- [ ] `tools/flash-usb.py --status` reports the application and its
      `sys.version`.
- [ ] Native only: microphone capture and speaker playback still work
      alongside (see [native/README.md](../native/README.md)).

## 9. Against a host: `tools/deck-sim.py`

`tools/deck-sim.py` stands in for the missing deck daemon. It needs the device
connected to this Mac over Bluetooth and `pip install hidapi`; it opens the
first device with the deck's VID/PID, `1209:5754` (`--vid`/`--pid` override).

```sh
python3 tools/deck-sim.py --dry-run                 # framing only, no device
python3 tools/deck-sim.py                            # until Ctrl-C
python3 tools/deck-sim.py --flood 200                # spinner title flood after the sync
python3 tools/deck-sim.py --send '{"m":"set","k":"brightness","v":2}'
```

- [ ] On start the demo clears, the header shows the Mac's clock, the sim's
      roster arrives, and Settings → Host → Deck host reads Present. The sim
      prints the device's `hello` and every event it sends.
- [ ] Answer a card: the sim prints `answer`, clears the card and puts the
      agent to work.
- [ ] `1`/`2` on the roster: the sim prints `channel` and answers `focus`.
- [ ] PTT on the roster: the sim prints `channel` (if it changed) and both
      `ptt` edges, then sends a transcript and a reply.
- [ ] Omarchy keys print `key` press/release pairs with the same `shift`;
      HOME HOME then a reorder prints one `faces`.
- [ ] `--flood 200`: the Working row updates without stalls; the sim prints no
      `err` and the HID link stays up.
- [ ] `--send` brightness 2: the panel dims at once and no `set` echo is
      printed.
- [ ] Stop the sim: after 15 s every face shows `offline`, dots turn gray,
      cards say offline, and PTT shows NO HOST. The demo does not come back.
      Restart the sim: it resyncs.

## 10. Rev A build on the development board

Only to trial the rotated path on the dev board. On the Rev A board itself the
`hw-a` image drives the real pins (panel rails, touch on PA42/PA41, the six
buttons and the wheel on GPIOs); its first power-on is the bring-up guide's
job, and sections 1–7 then apply unchanged with the real controls in place of
the keypad chords.

Recovery, on the Rev A board with the `native/bootloader` image in its slot
(the UART log shows `OWTB: OpenWalkieTalkie bootloader 1 at 0x20020000` after
`SFBL`):

- [ ] `tools/flash-usb.py <elf>` with the deck running: the log shows `power:
      rebooting into USB recovery` and `OWTB: recovery mode (requested by the
      application)`, the deck re-enumerates as the same "OpenWalkieTalkie" device
      (no prompt), the script downloads sector by sector, `OWTB: image
      written, vector table valid`, the deck reboots and the script reports the
      application's version.
- [ ] Hold SHIFT and ESC, then wake or power the deck: `OWTB: recovery mode
      (SHIFT+CANCEL held at power-on)`; `tools/flash-usb.py --no-request <elf>`
      flashes it. Leave it alone instead: after two minutes it boots the
      application by itself.
- [ ] Erase the application's first sector over the UART pads (`sftool ...
      erase_region 0x12020000 0x1000`) and reboot: `OWTB: recovery mode (no
      valid application image)`; `tools/flash-usb.py --no-request <elf>` brings
      the application back.

- [ ] Native `OWT_BOARD=hw-a tools/build-native.sh` (or Embassy
      `cargo run --release --features hw-a`); the log shows `rotation=180deg`
      and Settings → About names `Rev A`. A Rev B board takes `hw-b`
      instead (About names `Rev B`; `tools/flash-usb.py` refuses the other
      board's image once the deck runs a build that reports its board).
- [ ] Held upside down, the board shows everything upright. Repeat the corner
      taps, the top-edge swipe (from the edge that is now on top), one slide
      each way, and a sleep/wake frame from sections 1, 3 and 7.
- [ ] Flash the default image again before handing the board on.

## 11. Against `deckd` (host service, weeks 1–6)

The daemon's device checks of [host-service.md](host-service.md) §14. Build
with `tools/build-host.sh`; `tools/host-test.sh --hardware` runs the first two
rows by itself, then every week-2 row below except the ENTER one (§7.5, which
needs a hand on the deck). The week-3 rows at the end are by hand only: they need
real CLI sessions, and no gate may start one. Every number comes from `status.json` (written at shutdown) or
`deckd device metrics` (the control socket needs a data dir path under 104
bytes: `DECKD_DATA_DIR=$TMPDIR/deckd-hw`).

- [ ] `tools/host-test.sh --hardware`: 60 s of `deckd run --demo` answers
      every 5 s hello (≈12 replies), one barrier, `synced`, zero link drops
      and zero device `err`; the restart run gets a new sid, one barrier and
      `synced` again.
- [ ] Same-sid resync: `deckd device sync` writes a second barrier with the
      same card ids (`state.json.card_ids` unchanged); the deck raises no
      duplicate notice. A daemon restart allocates fresh ids instead
      (`last + 1`), so its cards are new to the deck by design.
- [ ] 30-min flood soak: `deckd device flood 8000` after the sync; the link
      stays up (`link_drops=0`), the deck refuses nothing (`err=0`), hello keeps
      answering, and the closing `write avg` is the BLE ack cadence
      (≈300 ms), not the 8 ms pacer.
- [ ] Non-seizing clients: `host/spikes/hid_probe.py --hello 0` next to the
      daemon counts the same input reports as the daemon's `rx` delta.

Recorded 2026-09-19 (firmware 0.1.0, macOS 27.0, hidapi 2.6.7, ChatGPT.app
running throughout): gate first run sid 4056446807 hello 12/12 misses 0
barriers 1 (12.7 s for 21 lines / 46 reports) drops 0 err 0 write avg 279 ms
max 419 ms; restart sid 3415025559 hello 5/5 barriers 1 synced drops 0 err 0
write avg 308 ms. Same-sid `device sync`: barrier 14 s, `card_ids {1:1, 2:1}`
before and after. Icon sync: layout flip pushed in 1.6 s, revert in 1.4 s,
`icons synced=2 errors=0`. Third client for 60 s: probe 76 reports (ch5 72 =
12 hello replies × 6, ch2 3, ch3 1) = daemon rx 52 → 128 and ch2 +3 over the
same window. Flood soak, 35 min: 5900 titles (≈2.8/s, the BLE ack cadence),
hello 413/413 misses 0, link drops 0, reopens 0, device `err` 0, decode/encode
errors 0, ch2 105, write avg 327 ms max 407 ms; reports tx 6429 rx 2620; the
UART log shows 5665 reports received, uptime monotonic, `power … errors=0`
in all 61 windows.

### Week 2 rows (API doors, holds, the hid writer thread)

`HOST-CONTRACT-W2` §7. `tools/host-test.sh --hardware` now drives these rows
itself (`hw_week2`, printing one `ok`/`FAIL` line each) on a short data dir of
its own with `[api] port = 0`; what follows is the same sequence run by hand,
which is still how §7.5 (ENTER on the deck) is done. Run against the real deck
with `deckd run --foreground --demo` on a short `DECKD_DATA_DIR`
(e.g. `/private/tmp/deckd-hw`). Nothing here flashes firmware, and
`tools/monitor.py` is attached to `/dev/cu.usbmodem*` one process at a time
and never while anything else opens the UART.

- [x] Demo roster: `deckd ctl agent.list` shows the six demo agents and the
      deck shows channel 6 `cohdl-api` with the held permission card.
- [x] Guarded allow: `deckd ctl --scopes operate op.answer '{"agent":<cohdl>,"card":1,"choice":0}'`
      → `-32002` and exit 3, `deckd events --kinds op.*` prints
      `op.confirm_requested`; `"choice":2` is accepted, the demo writes the
      hold, the deck clears the card and the card is back 3 s later.
- [x] Poll delivery with a waiter: the Python virtual agent over TCP on slot
      7, `op.answer` from `deckd ctl` while its `agent.wait` is parked →
      `metrics.get.answer_to_clear_host_us` < 100 000, `answer_to_clear_wire_us`
      recorded (BLE), the deck clears the card.
- [x] Poll delivery with nobody waiting: after 4 s `agent.get` says `failed`,
      the deck shows "not delivered" and reverts; the next `agent.wait` picks
      the answer up, the deck gets its `card clear` and the card is
      `delivered` (late pickup).
- [ ] ENTER on a demo card from a real device edge: `answer_to_clear_host_us`
      and `_wire_us` from a hand on the deck.
- [x] Week-1 rows with the writer thread: `write_avg_ms`, `link_drops=0`,
      `device_err=0`, a new sid resyncs after a restart.

2026-09-19 — **run** (firmware 0.1.0, macOS 27.0, hidapi 2.6.7, deckd at the
week-2 integration commit, icon-sync LaunchAgent stopped for the run and
restored afterwards — verified running again, pid 41547; nothing flashed, no
monitor attached).

- **Roster and link.** `deckd run --foreground --demo`: link up, boot barrier
  written in 17.5 s (21 lines / 46 reports at the BLE ack cadence), `synced`,
  session `live`. `agent.list`: six agents — slots 1–5 the mockup roster,
  slot 6 `claude/cohdl-api` `question` with card 1 `pending`/`hold`/
  `permission` (the design §4.13 agent added in week 2).
- **Guarded allow.** `op.answer{choice:0}` from `deckd ctl` (an untrusted UDS
  peer with `operate`) → `{"code":-32002,"message":"forbidden","data":
  {"how":"gui_confirm","need":"trusted"}}`, exit 3 with the CLI's hint, and
  exactly one `op.confirm_requested` on `deckd events`. `{"choice":2}` (Deny)
  → `{"delivered":true,"rev":36,"via":"hold"}` in **246 ms** (the demo's hold
  resolved and its response reported written), the card cleared on the deck,
  and the demo re-set a fresh permission card (id 2) 3 s later.
- **Poll with a waiter.** `host/examples/virtual_agent.py` over TCP took slot
  7; with its `agent.wait` parked, `op.answer{choice:0}` returned
  `{"delivered":true,"rev":49,"via":"poll"}` in **28 ms** (answer → delivery
  confirmed, §4.9), the script printed
  `{"card":1,"choice":0,"kind":"answer","label":"Yes",…}` and exited 0; the
  deck cleared the card.
- **Poll with nobody waiting.** `op.answer{choice:1}` returned
  `{"delivered":false,"reason":"timeout","via":"poll"}` after **4 128 ms** (the
  4 s budget), `agent.get` showed the card `failed` and the deck reverted it;
  the late `agent.wait` 25 s later returned the answer
  (`choice:1,label:"No"`), the card was cleared on the deck and dropped from
  the store (`card_after_pickup: null`). Event counts for the run: 3
  `card.answered`, 3 `card.delivered`, 1 `card.failed`, 3 `card.cleared`, 1
  `op.confirm_requested`.
- **Writer thread (the week-1 rows again).** Two runs plus a restart: hello
  27/27 and 16/16 replies, misses 0, one barrier each, **link drops 0**,
  **device `err` 0**, decode/encode errors 0, `write_avg_ms` 271–305,
  `write_max_ms` 376, `writes_in_flight_max` **1** (the hid thread hands one
  report to `deckd-hid-write` and never waits), `write_stalls` 0. Restart:
  new sid (211292554 → 1836630733), one barrier, `synced`, session `live`.
- **Open.** `answer_to_clear_host_us` / `_wire_us` are still 0: they are
  measured from a **device** `answer` input report, and every answer in this
  run came from `deckd ctl` over the API. The ENTER row needs a hand on the
  deck; the host-side halves are covered off-device by
  `core_delivery::answer_to_card_clear_under_100_ms` (1–3 ms host path),
  `core_delivery::forced_delivery_failure_then_late_poll_pickup`,
  `deckd::core::tests::op_answer_answers_after_the_delivery_or_the_budget` and
  `trust_api::token_and_uds_allow_on_a_permission_card_are_refused`.

### Week 3 rows (the adapters: real CLI sessions on the deck)

`HOST-CONTRACT-W3` §8. Nothing here is automated — these rows are the only
proof that the hooks reach a terminal somebody is sitting at. **Every one of
them runs against throwaway CLI config directories**: the user's own
`~/.claude`, `~/.codex` and `~/Library/LaunchAgents` are never written by a
test, and `shasum -a 256 ~/.claude/settings.json ~/.codex/hooks.json` before
and after the session is how that is proven rather than assumed.

```sh
export DECKD_DATA_DIR=/private/tmp/deckd-hw              # short: the socket path limit is 104 bytes
export CC="$TMPDIR/owt-hw-claude" CX="$TMPDIR/owt-hw-codex"   # absolute paths, and never ~/.claude, ~/.codex
mkdir -p "$CC" "$CX"
deckd run --foreground                                    # its own window; no --demo, the roster is the CLIs
CLAUDE_CONFIG_DIR="$CC" deckd integrations install claude # mints the `hooks` token in $DECKD_DATA_DIR
CODEX_HOME="$CX"        deckd integrations install codex  # then its /hooks review, row 5
```

Set `CODEX_HOME` per command, never for `deckd run` itself.

- [x] **A real Claude session on the roster.** Hooks that fire before the
      model call need no login, but a *bare* `CLAUDE_CONFIG_DIR` prints
      `Not logged in · Please run /login` and fires nothing at all, so use the
      dummy key and a dead base URL:
      `CLAUDE_CONFIG_DIR="$CC" ANTHROPIC_API_KEY=sk-ant-not-a-real-key ANTHROPIC_BASE_URL=http://127.0.0.1:47399 claude -p 'hello' --debug-file "$TMPDIR/owt-hw-claude.log"`
      (nothing listens on 47399, so the turn fails locally and nothing leaves
      the machine; `--debug-file` is the reliable trace — `--debug hooks` on
      stderr is lost when the process is killed). `deckd events --kinds
      'agent.*'` shows the register within 100 ms of the POST, `deckd ctl
      agent.list` has the session, and the log carries `Hooks: HTTP hook POST
      to http://127.0.0.1:47201/v1/hooks/claude` and `Hook SessionStart:startup
      (SessionStart) success:`.
- **The title beacon — retired 2026-11-13** with the Ghostty tab path (the
      hand-off rows below replaced it): `SessionStart` is answered `{}` and
      writes no title, so there is nothing to look for. The row was: straight
      after that session starts, before the next shell prompt,
      `osascript -e 'tell application "Ghostty" to get name of every terminal'`
      shows `deck:<id4>` for its tab.
- [ ] **A real permission prompt becomes a card.** Rows below need a model
      turn, which the dummy key cannot give: log in *inside the throwaway
      config* (`CLAUDE_CONFIG_DIR="$CC" claude`, then `/login`) and never copy
      credentials out of `~/.claude`. Ask for something that needs Bash ("run
      `ls /` with Bash"). The card appears on the deck; `deckd ctl --scopes
      operate op.answer '{"agent":"<id>","card":<n>,"choice":2}'` (Deny) →
      `{"delivered":true,"via":"hold"}` and the deny shows in the terminal;
      `'{"choice":0}'` (Allow) → `-32002 {need:"trusted"}` and exit 3, the
      week-2 guard — press Allow **on the deck** for the allow path and the
      command runs.
- [ ] **A 3-question `AskUserQuestion`, card by card.** Prompt Claude to ask
      three questions with the AskUserQuestion tool. Each question is its own
      card under one budget, **nothing is drawn in the terminal for the whole
      hold** (unlike a permission prompt, this hook does suppress the dialog),
      each answer clears its card before the next arrives, and the answers land
      in the session after the last one — a multi-select answer as a
      comma-joined string. `deckd events --kinds 'card.*'` shows one
      `card.answered` and one `card.cleared` per question.
- [ ] **Snooze degrades under the same id.** With a permission card up, press
      CANCEL on the deck (or `deckd ctl --scopes operate op.snooze
      '{"agent":"<id>","card":<n>}'`): the terminal dialog appears at once, the
      drawer entry becomes `⌨ …` with the one choice **Open app** under the
      **same** card id, and the deck does **not** chime again. ENTER on it
      brings the session's app forward and clears honestly.
- [ ] **Codex, including its trust gate.** `CODEX_HOME="$CX" codex` in a
      directory Codex has not seen: answer the directory-trust prompt first,
      then the hooks-review modal ("Trust all and continue", or `/hooks` → `t`
      on deckd's entry). `CODEX_HOME="$CX" deckd integrations status codex`
      goes from `untrusted` to `installed`, and only then do hooks run at all.
      The session reaches the roster on its **first turn** (not when the window
      opens); a command that needs approval becomes a card, and Deny from
      `deckd ctl` reaches the terminal with its message.
- [x] **install/uninstall leaves the throwaway dirs byte-identical.**
      `shasum -a 256 "$CC"/settings.json "$CX"/hooks.json "$CX"/config.toml >
      "$TMPDIR/owt-hw.sha"` before installing, then install, run the rows, and
      `deckd integrations uninstall claude` / `… uninstall codex`;
      `shasum -a 256 -c "$TMPDIR/owt-hw.sha"` passes and
      `ls "$DECKD_DATA_DIR/backups"` holds a backup per file per write — the
      install takes one and the uninstall takes another, so a full round trip
      leaves two per file, and nothing is left beside `$CC`/`$CX`. While the
      hooks are installed,
      `CLAUDE_CONFIG_DIR="$CC" claude doctor | grep -c 'Invalid settings'` is
      **0** — grep the output, never the exit code: `claude doctor` exits 0
      even on a file it has rejected.
2026-09-21 — **two rows run off-device** (no deck attached, the icon-sync
agent never stopped, `deckd run --no-device`), which is as far as these go
without a hand on the deck. A real `claude 2.1.278` turn in a throwaway
`CLAUDE_CONFIG_DIR` (with `ANTHROPIC_API_KEY=sk-ant-not-a-real-key` and
`ANTHROPIC_BASE_URL` pointed at a local 401 stub, per the C24c recipe above,
so hooks fire and nothing leaves the machine):

- The `SessionStart` **command** hook ran and Claude accepted its output
  verbatim — `{"hookSpecificOutput":{"hookEventName":"SessionStart"},`
  `"terminalSequence":"\u001b]2;deck:42fc\u0007"}` — which is `deckd hook
  claude`, the parent walk and the C25 one-shot beacon, end to end.
- Both http hooks answered `200, body length 2`: `{}` first, as §4.11 requires.
- The roster, sampled **while the session was alive**: one agent,
  `claude/<name>` `state=working` `slot=1` `source=hook`
  `key=claude:<the real session UUID>` `pid_kind=cli`; it was released when
  the session ended.
- `integrations install claude` → `uninstall claude` left the throwaway
  `settings.json` byte-identical, with the backup under `<data>/backups/`.

The Codex half of the same row and the `claude doctor` check were run
separately the same day, against `claude 2.1.278` and `codex-cli 0.153.4`,
with `HOME`, `CLAUDE_CONFIG_DIR` and `CODEX_HOME` all inside one throwaway
tree and the daemon's `--data-dir` beside them:

- `install claude` + `install codex` wrote three files (`settings.json`,
  `hooks.json`, `config.toml`); `uninstall claude` + `uninstall codex` then
  brought all three back, and the `shasum -a 256 -c` taken before the first
  install passed on **all three**, not just the Claude one.
- `claude doctor`, run against a *copy* of the written config dir so the CLI's
  own scratch writes could not pollute the diff, printed `Invalid settings`
  **0 times** (`Running: native (2.1.278)`). The exit code was 0, as it is
  even for a file Claude has rejected, so only the grep counts.
- `integrations status` in between reported `claude installed` and `codex
  untrusted` — the latter with the `/hooks` review step spelled out — and
  wrote nothing; `codex untrusted` here is trust read out of `[hooks.state]`
  in `config.toml`, with no `codex app-server` started.
- `<data>/backups` held six files, two per installer-owned file (one from the
  install, one from the uninstall), and `find "$CC" "$CX"` turned up no
  backup or `.bak` stray beside either config dir.

Two defects only a live session could find, both fixed here:
`~/.local/bin/claude` is a symlink to `…/claude/versions/2.1.278`, so the
process calls itself `2.1.278` and the basename match missed it — every
natively installed CLI was `pid_kind: unknown` (never liveness-polled) and
escaped the §4.2 ancestry cap (the cap's own walk still read names only
until 2026-11-13, when it started reading each hop's executable path too);
and the installer wrote `[api] port` rather
than the port the daemon bound, so an ephemeral `port = 0` baked
`http://127.0.0.1:0/v1/hooks/…` into the user's settings.

Still needing a hand on the deck: a real permission card, the 3-question
group, the snooze degrade, and Codex's `/hooks` trust review (the title
beacon is retired 2026-11-13, above).

- [ ] **Teardown.** `rm -rf "$CC" "$CX"`; the `shasum` of the user's own
      `~/.claude/settings.json` and `~/.codex/{hooks.json,config.toml}` is
      unchanged from before the run.

### Week 4 rows (voice: the deck as an audio device)

`HOST-CONTRACT-W4` §6. These need the deck connected as an **audio** device
(not only BLE HID) — `system_profiler SPBluetoothDataType` must list
`OpenWalkieTalkie` under `device_connected` with `A2DP` and `HFP` among its
services. The rows that open the microphone are behind a second opt-in,
`OWT_HW_MIC=1`, because opening it can raise a TCC prompt for whichever app
is *responsible* for the process (V9), and a test suite may not put a system
dialog in front of someone unasked.

```sh
OWT_HW_MIC=1 cargo test --manifest-path host/Cargo.toml --target "$(rustc -vV | sed -n 's/^host: //p')" \
  --target-dir target/host --locked -p deckd --test voice_hardware -- --ignored --nocapture --test-threads=1
# or, as part of the gate:  tools/host-test.sh --hardware
```

- [x] **The deck is found by UID, not by luck.** Both directions enumerate and
      `cpal`'s own `Device::id()` round-trips.
- [x] **A short, quiet chime reaches the deck** through the mixer's chime lane.
- [x] **SCO bring-up and A2DP resume, in milliseconds**, from inside the test
      suite rather than a throwaway probe.
- [ ] **A real PTT answer round trip on the deck** (press → speak → release →
      the card is answered). Needs a hand on the deck and a logged-in CLI.
- [ ] **Barge-in heard**: TTS playing, PTT pressed, the tail audible for ~0.5 s
      (V6 — the mixer is silent within 35 ms; the Bluetooth pipe is not).
- [ ] **`set speaker` / `set mic` audibly change gain** from the deck.
- [ ] **The Codex-app coexistence case** (both holding the mic) — could not be
      exercised: the Codex desktop app registers no audio client on this Mac.

2026-09-21 — **the three automated rows ran and passed**, and they reproduce
the contract's own measurements from inside the suite:

```
deck uid   C2-2D-81-AB-88-81
cpal Input : coreaudio:C2-2D-81-AB-88-81:input    nominal 16000, alive
cpal Output: coreaudio:C2-2D-81-AB-88-81:output   nominal 44100, alive
chime: rendered 11776 frames into C2-2D-81-AB-88-81:output at 44100 Hz
first input callback :   103.34 ms   (40 buffers)
first NON-ZERO sample:   883.34 ms      <- V2: the callback is 9x early
output nominal during capture: 16000     <- the SCO flip
A2DP resume to 44100:  2411.46 ms after close   <- V1: ~2.3 s, a fixed timer
```

That the first callback arrives 9× earlier than the first real sample is the
whole of V2: a dead-mic check that counts buffers can never fire, because a
dead mic delivers a second of digital silence exactly like a live one.

**Not claimed here:** nothing about the daemon's own TCC identity. These rows
run from a terminal and borrow the terminal app's microphone grant (V9); a
launchd-spawned `deckd` gets its own row and inherits none of it. That is
week 7's gate, and `perm.request` is stubbed until then.

### BLE voice rows (Rev A Embassy image, no Bluetooth audio profile)

The Rust image's voice link (`docs/deck-protocol.md`, "Voice"). Bench runs
of 2026-10-04 used a CoreBluetooth probe against the `audio-open` build, so
the link was not encrypted; the encrypted rows need the deck paired with the
Mac first.

- [x] **The deck is found by its audio service.** The 128-bit service UUID
      rides in the scan response; the info characteristic reads
      `1 1 16 20 3` (version 1, IMA ADPCM, 16 kHz, 20 ms, mic + speaker).
- [x] **macOS grants the link a stream needs.** Data length 251/251 bytes,
      2M PHY both ways, ATT MTU 247 (a full 166-byte frame per notification).
- [x] **A microphone stream arrives whole.** MIC on for 5 s: START, 245
      AUDIO frames of 166 bytes, END with reason `limit`; no sequence gaps,
      none dropped in the deck. The PDM FIFO packs two samples per DMA word
      (seen in the first capture; the driver now assumes it); a quiet room
      reads -70 to -76 dBFS RMS.
- [x] **The fast link removes the latency stalls.** At macOS's idle
      parameters (15 ms, latency 22) the mic stream stalled 345 ms every
      ~1.5 s; with the deck's fast-link request (15 ms, latency 0) the
      largest gap between notifications was 43 to 59 ms, and the deck asks
      for the old parameters back 5 s after the last audio.
- [x] **The speaker plays at the right pitch.** A 1 kHz loopback tone came
      back through the deck's own microphone at about 1 kHz. This run also
      showed that the DAC's 16-bit mono mode takes two samples per 32-bit word
      (the HAL documents one), so the stream played at half speed; fixed in
      `src/audio/speaker.rs`.
- [ ] **The fixed speaker path, end to end.** Still to be measured on a
      paired deck: every connection to the unpaired deck brought in macOS's
      HID service, which raised a pairing request and held the probe's
      writes until it timed out.
- [ ] **Encrypted, with the deck paired.** Mic and speaker on the default
      build (no `audio-open`), over the system's own HID connection.
- [ ] **PTT to text and back.** A PTT press streams to deckd, STT hears the
      words, and the reply's TTS plays on the deck's speaker.

### Hand-off rows (2026-11-13: the session's own app, on a press)

The owner's check on the **installed** build: no automated test may raise an
app, and whether macOS lets the daemon's launch agent bring one forward is
exactly what is not known. Open background Claude Code sessions in Zed,
Ghostty, Terminal, iTerm2 and Claude Desktop, then:

- [ ] **ENTER on an `Open app` card brings that session's app forward.** Get
      one by letting a held permission pass `hold_s`, or by pressing CANCEL on
      it; Claude's own `Notification` cards are the other kind. `deckd.log`
      says `desktop: hand-off … outcome=raised`.
- [ ] **A channel change and a PTT press raise nothing.** Turn between
      sessions and dictate to one in the background: no app comes forward.
- [ ] **An app that has quit is not launched.** Quit the app a session ran
      in (leave the CLI running under `tmux`), press its card: nothing is
      launched, and the deck shows "Not opened: its app is not running." in
      its language.
- [ ] **If nothing comes forward** while the log says `raised`, macOS ignored
      `open -b` from the launch agent: swap the order in
      `deckd::desktop::raise::MacRaiser::raise` (`activateWithOptions`
      first) and repeat.

### Delivery record rows (2026-11-13: are the proxies honest?)

The record (`docs/host-service.md` §10.6) reads "reached", "deck used" and
the reader's opens off the lines firmware 0.13.0 sends; only a session on a
real deck says how close those come. Read it before and after each row with
`deckd ctl --scopes admin metrics.get` (the `deliveries` object) and compare
the counters, and the last entry of `recent`, with what you did:

- [ ] **A card answered on the deck.** Answer a permission card with ENTER:
      the newest record says `reached`, `deck_used`, `answered:"deck"`,
      `outcome:"answered"`. Answer the next one by voice: `"ptt"`.
- [ ] **A card you only looked at.** Let a card come and go (answer it in the
      terminal) while you watch the deck and press nothing: `reached`,
      **not** `deck_used`, `outcome:"elsewhere"` — the known limit of the
      proxy. Note how often this is what really happened.
- [ ] **The reader.** Open a session with ENTER (`channels` +1), dictate to
      another one (`channels_by_ptt` +1, not `channels`), scroll a long reply
      past its first window (`history` +1 however long you scroll), open a
      tool row (`tool_rows` +1) and an image (`images` +1, even when the
      first chunk is retried), open the model picker (`pickers` +1).
- [ ] **Nothing it should not have.** `grep` the support file and
      `state.json` for a word from a card, a reply and an order you sent
      today: none is in the delivery record.

### Week 5 rows (the desktop: Ghostty, focus and injection) — retired

**Retired 2026-11-13** with the Ghostty tab path (the hand-off above
replaced it); the rows are kept as the record of what was measured, and
`tools/host-test.sh` no longer runs a `desktop hardware` step.

Run by `tools/host-test.sh --hardware` (the `desktop hardware` step). Ghostty
1.3.1, Automation TCC already granted to the responsible process, 2026-09-22.
These are the rows that can only be true against the real app; every resolver
and injector *path* is an ordinary counted `#[test]` against the scripted
`Scripting` fake and needs no Ghostty at all.

| # | Row | Result 2026-09-22 |
|---|---|---|
| 5.1 | The `deck:<nonce>` beacon is found on `terminal.name` after an OSC 2 write to the tty | **pass** |
| 5.2 | `perform action "set_tab_title:…"` **outlasts a later OSC 2** | **pass** — `tab.name` still `deck:5ddf` after `ESC ]2;HOSTILE_RETITLE BEL` |
| 5.3 | `terminal.name` *follows* that same OSC 2 — the two fields are distinct | **pass** |
| 5.4 | An already-pinned session is **re-resolved on `tab.name`** once the beacon is overwritten | **pass** — with the surface id the daemon recorded; a lone label with nothing on file is refused on purpose (see below) |
| 5.5 | Unpinning with the empty value restores the live title | **pass** |
| 5.6 | `input text … to:` + `send key "enter" to:` reach the **aimed** surface while another is selected | **pass** |
| 5.7 | …and the aimed text does **not** leak into the other surface | **pass** |
| 5.8 | `send key "return"` is still refused by this Ghostty | **pass** — `-1700 Unknown key name: return`, re-measured live on every run |
| 5.9 | Guard 3 allows the pid holding the foreground pgrp of its tty | **pass** |
| 5.10 | Guard 3 **refuses a live pid that is not the foreground pgrp** (the "exited to a shell" case) | **pass** |
| 5.11 | Guard 3 refuses a dead `cli_pid` | **pass** |
| 5.12 | Guard 3 refuses a pid that is foreground on a **different** tty | **pass** |
| 5.13 | The **whole injector chain** resolves, pins, guards and types into the aimed tab | **pass** |
| 5.14 | `op.focus` selects the resolved tab, and the unpin leaves the title as it was found | **pass** |

Row 5.4 is worth reading twice. A resolver that adopts any lone `deck:<id4>`
it finds is the one that types into the wrong terminal: `id4` is a function of
the agent sequence alone, so the first agent of **every** daemon run wears
`deck:0001`, and a `kill -9` leaves that label on a tab for the next run to
find. The row therefore passes only with the surface id the daemon actually
recorded — confirming, never guessing.

The two corrections these rows exist to catch, both of which would have shipped
silently:

```
send key "return"   ->  -1700 Unknown key name: return     (the vocabulary is `enter`)
tab.name  = deck:325d          <- the pin, an override, survives OSC 2
term.name = HOSTILE_RETITLE    <- the live title, does not
```

`send key "return"` is what §5.5 specified until today; it fails every
injection. And because the pin lives on `tab.name` while the beacon lands on
`terminal.name`, a resolver that reads one field finds a fresh session but can
never re-find a pinned one.

**The row that is missing, and why it matters.** None of these drive a real
`claude`. All four create `exec cat` surfaces and write their own beacon with
the same helper the resolver reads with — which is exactly why the week-5
review's worst finding (the resolver searching for `deck:deck:<id4>`, so that
nothing could ever be pinned or injected into) was invisible to them. A
cross-seam unit test now drives the real Claude adapter's own output through
the real resolver, which is the stronger guard for that particular bug, but
§15's first week-5 exit test — "PTT dictation lands in a Claude session via
Ghostty", with a real Ghostty and a real logged-in `claude` — is **not
implemented** and is not claimed. It is the one row that would have caught it
from an edge.

**Not claimed here:** the `-1743` Automation denial and the `-1712`
wake-and-retry path. Automation was already granted to the responsible process
on this machine, so neither could be provoked live; both are implemented and
unit-tested against injected errors only. Nor is the ~118 ms TOCTOU window
between guard 3's last check and the Return closed — it is measured and
documented, and shrinking it means in-process AppleScript instead of spawning
`osascript`. Rows needing a hand on the deck (a real PTT dictation into a live
Claude session, and device `channel` focusing the right tab with two agents
pinned) are listed in the week-5 gate and are **not** claimed by these rows.

### Week 6 rows (the three methods the GUI calls)

`config.set`, `service.restart`/`service.stop` and `voice.engines`/`voice.test`
(HOST-CONTRACT-W6 §3). These are not device rows — none of them needs the deck
in hand — but two of them are **process** rows that no in-process rig can
speak for, and one of them reads the real Apple engines, so they are recorded
here with the rest. Run 2026-09-22, macOS 27.0, this machine.

| # | Row | Result 2026-09-22 |
|---|---|---|
| 6.1 | `service.restart` over the daemon's own socket: the caller **gets its reply**, then the process exits **75** | **pass** — `tests/service_exit.rs`, the real binary spawned as launchd spawns it (`run --foreground --no-device`, a throwaway `--data-dir`, `[api] port = 0`); the reply carries `{action:"restart", exit:75, pid}` and `pid` is the child's, `try_wait()` says the process was still alive when the line was read, and the exit code is 75 |
| 6.2 | `service.stop` the same way, exiting **0** | **pass** — same rig; the socket is removed on the way out |
| 6.3 | A session without `admin` cannot stop the service | **pass** — `-32002 {need:"admin"}`, and the daemon is still serving four grace windows later |
| 6.4 | `config.set {patch}` writes the operator's file and the **running daemon** agrees with it | **pass** — `tests/config_set.rs`: the real `notify` watcher on a temp data dir, `config.set{patch:{"voice.stt.locale":"en-GB"}}`, then `config.get` (answered by the core, from `Core::config`, which only `CoreMsg::Config` sets) reads back `en-GB`, and `config.changed{keys}` is on the bus. A two-key patch lands the same way |
| 6.5 | …keeping the comments and writing nothing else | **pass** — the file still opens with the operator's comment line, no default was materialised, and a refused patch leaves the bytes identical with no temp file in the watched directory |
| 6.6 | `config.set` refuses the trust boundary, and a patch is **all-or-nothing** | **pass** — `[trust] require_trusted_for_allow`/`dev_requirement`, `[desktop] inject`, `[api] allowed_origins`, `[privacy] log_content`, `[voice.stt] allow_server`: each `-32602`, each with the file unchanged; and `{api.port, trust.require_trusted_for_allow}` in one patch writes **neither** — the port must not move while the rest is refused, or the hooks point at a port nothing answers on |
| 6.7 | The engines report `audio::spawn` posts really lands, read from the **real** frameworks on `main` | **pass** — `tests/voice_mainloop.rs` (`harness = false`, so its `fn main` *is* the main thread): stt `apple`, 62 locales, `on_device true`; tts `apple`, 49 locales |
| 6.8 | `voice.engines` answers from that report with no main-thread hop | **pass** — `core::dispatch`'s unit row feeds a `FromAudio::Engines` and reads the answer off the cache; `-32013 degraded{reason:"voice_unavailable", why}` before it lands and in every build with no engine |
| 6.9 | `voice.test{what:"speaker"}` queues one short quiet tone and reports the route | **pass** — exactly one `AudioCmd::Chime{which:"ready"}` on the queue (110 ms, peak 0.2, the deck's own output UID, the operator's own `set speaker` gain) and nothing else |

**Not claimed here.** **`voice.test` does not exercise an engine.** `speaker`
queues a real chime into the real mixer, and that is the only `what` that makes
a sound; `stt` and `tts` report what the engine said about **itself** when it
came up, and `mic` does not open a microphone at all — a capture has an owner,
a 60 s cap and a TCC grant behind it (§7.4, §7.8), and driving a recogniser or
a synthesiser is the main-thread hop the core thread may not take. Every one of
those says so in its own `limits`. Nobody **heard** the chime in 6.9 either:
a chime carries no utterance id, so there is no `voice.utterance` report to
wait for and `played` means "the audio thread took it". A row for the tone
through the deck's own speaker, with the route it forced, belongs with the
week-4 voice rows and is not claimed.

6.1 and 6.2 are real process exits and are claimed as such. What is **not**
claimed is launchd's half: the plist was corrected from `KeepAlive: true` to
`{SuccessfulExit: false}` in the same pass — with `true`, exit 0 was restarted
and both codes meant nothing — but no row here installs the LaunchAgent and
watches launchd honour it, because that would replace the operator's own
running service. The plist is `plutil -lint` clean; the behaviour it buys is
on the week-7 install gate.

6.7 read 0 **enhanced** en-US voices on this machine, so §7.7's `default_pool`
still falls back to the kind default here; whether enhanced voices are
downloaded is a System Settings choice and the row prints it rather than
asserting it.
