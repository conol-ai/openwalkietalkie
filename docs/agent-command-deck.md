# Agent Command Deck

The interaction grammar as the firmware implements it. The source is the
reference: `src/ui/state/` (one file per layer or face, each with its rules in
its module comment), `src/deck/` (model, protocol, demo) and `src/power.rs`.
The wire side is [deck-protocol.md](deck-protocol.md). The product spec is the
[Agent Command Deck note](https://conol.ai/share/note/i5mpd4opi9yoiabb7uy0z3gf);
its eight mockups are in [mockups/](mockups/), numbered as the source comments
cite them ("mockup 6" is `6-roster.png`). The firmware's type is about 1.33x
the mockups' size, because their 11-12 px text is under 1 mm tall on this
38.8 mm panel; the layout rectangles follow the mockups.

Three channels, one job each: the screen is the status board, PTT is the radio,
the buttons are the decisions. The device shows who needs you and never the
work itself.

Controls below are the product's names. On the development board: SCROLL CW =
▼ (21), CCW = ▲ (41), ENTER = dot (31), CANCEL = ESC (42), SHIFT = HOME (43),
PTT = envelope (44), LEFT = `1` (32), RIGHT = `2` (33).

## Principles

- **Local actions fire on the press.** A release never triggers a local action.
- **Every press latches an owner.** A host key, a macro key or a local
  action. Whatever tears the context down (a slide, an overlay opening or
  closing, idle-home, a jump, a host push that removes the key, the deck host
  going away) sends that owner's release at once and swallows the physical
  release that follows. A HID link drop forgets the holds without sending
  anything; hosts release on a drop.
- **The user's view never changes under a confirm.** Whenever the card on screen
  changes for a reason the user did not cause, its choice resets and ENTER or a
  tap is ignored until 500 ms after the card appeared (and the press came after
  that).
- **SHIFT is a modifier.** It is never sent by itself; it travels as a `shift`
  flag on Omarchy keys. An unassigned SHIFT chord on a locally handled face is
  swallowed, never passed through as the plain key.

Wheel navigation uses **CCW for next/down** and **CW for previous/up** across
pages, menus and lists. Numeric settings still increase clockwise; game turns
and assigned Macropad bindings use physical wheel direction.

## Global layer

| Input | Effect |
|---|---|
| SHIFT+SCROLL | Next (CCW) or previous (CW) face. One face per detent, at most one detent per 150 ms, hard ends with one small bounce, no wrap. The strip eases in about 220 ms and the new face takes input immediately. On the dev keypad a plain SCROLL within 300 ms of the last SHIFT+SCROLL is swallowed (its key-repeat tail); the Rev A wheel has no repeat and every detent counts. Closes the drawer; ignored in the manager. Sends `face`. |
| Double-SHIFT | Two SHIFT taps, each shorter than 300 ms, the second starting within 400 ms, with no other control in between. Opens or closes the face manager. Ignored during a LISTENING session. |
| CANCEL at a face's top level | Home: the Agents roster. On Omarchy SHIFT+CANCEL is forwarded to the host. |
| Swipe down from the top edge | A touch starting in the top 48 px never reaches the face. 40 px down, more down than sideways, opens the notification drawer. |
| PTT | See [PTT sessions](#ptt-sessions). |

Programmatic jumps (home, a toast tap or ENTER, idle-home, auto-jump) cut
instead of sliding. Overlays capture every face control and nothing is sent to the host
while one is open, except PTT, which still works over the drawer and the
manager. Precedence: naming > remove confirm > manager/gallery > drawer > face.

## Faces

The default strip is Agents, Omarchy, Macropad, Usage, Settings (face
ids 1–5). Agents and Settings are pinned; the rest can be reordered, removed
and added in the manager, up to eight faces and three Macropads. Header tags on
the right: `demo` while the demo host is active, `offline` after the deck host
was lost, the host's clock otherwise; a battery at 15% or below replaces the
tag on every face with an amber percentage.

### Agents (home)

Four views: Roster, Channel, Question card, Ready card. Each agent is a
walkie-talkie channel; one is active at a time.

- **Roster.** One page per project (the host's `proj`: the directory the
  session runs in), the header naming it and its place among the pages
  ("COHDL · 1/2"; "OTHER" is the page of the agents without a project;
  "AGENTS" when the roster is empty). Rows show a state dot (red Question,
  green Ready, blue Working, amber Error, gray Idle), "Kind · name" — the kind
  alone when the name is the project's own — and the agent's window title
  verbatim (left blank when it is just the project's name), with the state's
  age while Working or Question if it fits. Pages
  come in name order, the unlabelled one last; inside a page the order is
  Question, Ready, Error, Working, Idle; a re-sort waits for 1.5 s without
  input, while dots and titles update in place at once. SCROLL moves the
  highlight down the page; LEFT/RIGHT turn to the neighbouring project's
  page (hard ends, its first row highlighted, nothing sent); ENTER opens the
  highlighted channel (`channel`). The active channel has a bar at its row's
  left edge, separate from the highlight. The host's `focus` always moves the
  bar, and the highlight — turning the page with it — only when nobody is
  using the roster (nothing held, 2 s without input). A page that empties
  gives way to its neighbour. Touch: tap a row to highlight, tap the
  highlighted row to open it.
- **Channel.** State line, conversation history (SCROLL browses it) and the
  pending card: a Question with exactly two choices shows both as buttons (tap
  answers), other Questions show "Answer · N choices", a Ready shows "Ready
  summary". ENTER opens the card, or switches Chat/Workflow when no card is
  pending; LEFT/RIGHT go to the neighbouring agents
  (across every page, in roster order); CANCEL goes back to the roster.
  Tool activity occupies one summary row per entry; tap its arrow or label
  to expand the details, and tap again to collapse. Live updates keep that
  choice. The conversation opens at the tail and follows incoming text while
  the end is visible. Scrolling away holds the reading position; scrolling
  back to the end or SHIFT+ENTER resumes following.
- **Workflow.** Tap **Workflow** in the channel's state row to see live plan
  progress and recent tool activity. Claude background workflows appear first,
  with the run name, successful/started workers, progress bar, elapsed time,
  and tokens; these continue updating while the parent chat is idle.
  The compact plan shows three steps around
  the current step; tap **Full plan** for all steps and the explanation.
  The latest three tools, running tools and expanded tools stay visible;
  tap a tool for its input/output, including syntax-highlighted code, or
  **All tools** for older activity. SCROLL browses the view; SHIFT+ENTER returns
  to the top. Tap **Chat** or press CANCEL to return to the conversation.
  Pending cards remain accessible and take priority on ENTER. PTT continues
  to address the selected agent. Progress comes from explicit plan statuses;
  sessions without a plan show activity without an invented percentage.
  This view requires both the updated host and device firmware.

- **Question card.** SCROLL chooses; ENTER or a tap confirms under the guard and
  sends `answer`. The card then shows "sent" and locks until the host clears it;
  with no clear in 5 s it reverts and a "not delivered" toast appears. PTT
  answers by voice. LEFT/RIGHT walk the other pending cards (Questions first,
  hard ends). CANCEL snoozes: the notice goes to the drawer and the view goes
  back to the channel. After the user's own answer the view returns to the
  channel; it never advances to another card by itself.
- **Ready card.** The summary and the suggested next steps as rows. SCROLL
  highlights a step and ENTER sends it as an `answer`; ENTER with no step
  acknowledges the card (its notice goes away) and opens the channel. CANCEL
  goes back to the channel. An acknowledged Ready is skipped by LEFT/RIGHT.
- Without a deck host (and outside the demo) cards are inert and say "offline";
  nothing is sent. A card or agent the host removes drops the view back to the
  channel or roster, never onto another card.

### Omarchy

A remote for the Omarchy (Hyprland) desktop: workspace chips and the window
list, as the host pushes them (`ws`). LEFT, RIGHT, ENTER and SHIFT+CANCEL are
sent as `key` press/release with the SHIFT state latched at the press;
SCROLL sends `key cw/ccw act:2` for next/previous window (physical CCW/CW)
and moves a matching local highlight until the next `ws`.
Tapping a chip sends `ws go`, tapping a window row `ws win`. Plain CANCEL goes
home. Header tag `remote`.

### Macropad

Direct binding, no cursor. Each of the nine keys may be bound by the host to a
hardware control (LEFT, RIGHT, ENTER, CW, CCW, SHIFT+LEFT, SHIFT+RIGHT,
SHIFT+ENTER); a bound key draws solid with a small binding glyph and that
control presses it (`macro` act 1/0; the wheel sends act 2 and flashes the key
for 150 ms). An unbound key is dashed and touch-only; a missing key is a dashed
"+ assign". Touch presses any present key on Down. A key pressed by finger and
binding at once is one press. Plain CANCEL goes home; CANCEL is never
bindable. The header tag names the host's source. A binding two keys share
goes to the first of them.

A key may carry a keystroke (`k`, e.g. `cmd+shift+4`): the key draws solid
with the Mac glyphs (⌃⌥⇧⌘ and the key) in its corner, and every press or
wheel step of it has the host post that keystroke to the desktop (deckd does
it through CoreGraphics; it needs the Accessibility permission, and the first
refused press makes macOS ask for it). A key with
neither a keystroke nor a hardware binding is dashed and only reports its
presses.

Keys are edited on the deck as well as by the host. A tap on "+ assign",
SHIFT + a tap on any key, or SHIFT+CANCEL (a cursor over the grid: SCROLL,
LEFT and RIGHT move it, ENTER opens the slot, CANCEL leaves) opens the key
editor, an opaque page with the key previewed and its rows: Label,
Modifiers, Key, Save, and Remove key for a slot that had one. SCROLL selects
a row; holding PTT dictates the label (a `name` session, as when naming a
face, and the host's `name` text lands in the draft); ENTER on Modifiers
adjusts the modifier set with SCROLL (none, ⌘, ⇧⌘, ⌥⌘, ⌃⌘, ⇧, ⌥, ⌃ … all
four; hard ends), ENTER on Key the key (none, a–z, 0–9, F1–F12, the arrows,
↵, esc, tab, space, bksp, del, home, end, pgup, pgdn, the punctuation; LEFT
and RIGHT jump ten); ENTER or CANCEL ends adjusting. ENTER on Save commits
(Save without a label only says so; modifiers without a key save as no
keystroke), on Remove clears the slot; CANCEL discards. A hardware binding the
host set is kept. A commit changes the deck's set at once and sends
`macrokey`; the host stores it and sends the whole `macro` back. No overlay
chord works inside the editor; idle-home discards it.

### Usage

Provider usage bars from the host (`usage`). SCROLL scrolls, ENTER sends
`refresh` and shows "updating…" for 1.5 s, CANCEL goes home. Bars are the
host's colour, else blue below 50%, green below 80%, amber below 95%, red from
95%. `deckd` fills it with the Claude and Codex accounts' rate-limit windows
(one bar each for the session and the week, with the countdown to the reset,
`docs/host-service.md` §4.10) and re-probes both on ENTER. When an unused
manual reset grant expires within 72 hours, both the Mac Usage page and the
deck show a small amber tip below the weekly bar (for example, "2 resets
expire in 1d 6h"). It counts grants at the nearest expiry, hides expired or
spent grants, and is separate from the automatic weekly reset countdown.
Claude supplies eligible grants in its optional usage response; Codex supplies
reset-credit details through the app-server. Missing eligibility or expiry
data produces no tip. Redemption remains in the provider's app.

### Timer

Add Timer through the face manager's gallery. Its three tabs are Stopwatch,
Timer and Pomodoro; LEFT/RIGHT moves one tab at a time with hard ends, and
tapping a tab selects it. Each clock keeps running when another tab or face
is shown, an overlay is open, or the screen is dark. A dot marks a running or
finished clock on another tab. A running clock keeps idle-home on this face;
the screen still dims and sleeps normally.

- **Stopwatch:** shows hundredths of a second. ENTER starts, pauses or
  resumes. CANCEL records a lap while running (the newest three are shown),
  or resets a paused stopwatch.
- **Timer:** SCROLL changes its length while idle, or tap a preset. ENTER
  starts, pauses, resumes or restarts a finished countdown; CANCEL resets.
- **Pomodoro:** a 25-minute focus session is followed by a 5-minute short
  break. After four completed focus sessions, the break lasts 15 minutes.
  ENTER starts, pauses or resumes. When a phase finishes it stays at zero;
  ENTER / Next starts the next focus session or break. CANCEL resets the
  whole cycle to the first focus session. Focus progress and the next phase
  appear below the readout.

Finished countdowns and Pomodoro phases wake the screen, raise a toast and
ring through the deck's speaker at its volume: four quick beeps a second for
the timer, a rising chime when a focus session ends and a falling one when a
break ends. The alarm rings for up to 30 seconds and stops at any touch of the
deck (a button, the wheel or the screen, including the press that wakes it),
or when the timer is restarted or reset or the next Pomodoro phase starts.
Tapping the toast opens the Timer face on the corresponding tab. The timers run
locally without a host, and reset when the device restarts. CANCEL with
nothing to reset goes home.

### Settings

The list grammar of every other face: SCROLL selects, ENTER acts. SCROLL moves
the selection through every row of every panel in order, the tab following it
(hard ends); LEFT/RIGHT or a tab jump to a panel's first row. ENTER on a toggle
flips it; ENTER on a value (speaker, mic, brightness, idle home, text size, language)
starts adjusting it, drawn with a blue outline: SCROLL then steps the value
(hard ends) and ENTER or CANCEL ends adjusting. Read-only rows ignore ENTER.
Leaving the face ends adjusting. A change applies locally and is sent once as
`set`; a host `set` overrides without an echo. Language is saved in device preferences and synchronized with capable apps
using its own versioned `language` message, without an ordinary `set` event. Touch: tap a row to select it,
tap the selected row to act as ENTER does. CANCEL (not adjusting) goes home.

| Panel | Row | Values | Default | Applied by |
|---|---|---|---|---|
| Volume | Speaker | 0–10 | 6 | host |
| Volume | Mic gain | 0–10 | 6 | host |
| Speech | Auto-announce | on/off | on | host |
| Alerts | Chime | on/off | on | host |
| Alerts | Auto-jump | on/off | off | device |
| Display | Brightness | 1–5 (panel 40/80/130/190/255) | 5 | device |
| Display | Idle home | never, 30 s, 1 min, 2 min | 2 min | device |
| Display | Text size | small, medium, large | medium | device and host wrapping |
| Display | Language | English, 简体中文, 繁體中文, 日本語 | English | device and app |
| Display | Tutorial | action: replays the tutorial | | device |
| Host | Link, Deck host | read-only status | | |
| Host | Demo data | on/off | off | device |
| About | Firmware, Battery, Power off | read-only (Firmware shows `0.1.0 · LilyGo` or `0.1.0 · Rev A`; Power off shows `hold CANCEL 3 s`) | | |
| About | Factory reset | action: the `Factory reset?` dialog | | device |

Display scrolls its four visible rows to reach Language and Tutorial. The
language choice applies immediately, survives a restart, and uses stable
ids `0=en`, `1=zh-Hans`, `2=zh-Hant`, `3=ja`. Built-in labels are translated;
host documents, custom names and physical keycap names are preserved. The
language revision is persisted on both sides: newer revisions win when reconnecting,
with the app winning ties. Each explicit local choice increments the revision.
Language sync is independent of `boot` settings restoration and speech recognition;
older firmware and older apps continue without the new message.

Host-side rows show "needs host" while there is neither a deck host nor the
demo. Turning Demo data off without a host clears the demo at once; turning it
on reloads it.

Two rows act on ENTER instead of adjusting. Display > Tutorial replays the
[tutorial](#tutorial). About > Factory reset opens `Factory reset?` over the
face (Keep / Reset, the remove-confirm card): ENTER or a tap on Reset sets
`Update::factory_reset`, CANCEL or Keep closes it, SHIFT+SCROLL and
double-SHIFT are ignored while it is up, and idle-home closes it. The
firmware then releases every hold and sends `up` as a power-off does, blanks
its flash journal (the Bluetooth pairing and the device preferences) and
restarts: the deck comes back unpaired and in the tutorial. The host's own
persisted strip and settings are untouched and come back on the next
`hello`. The dev board erases and resets the UI in place; the native image
shows the dialog but has no storage to erase.

## Face manager

Double-SHIFT opens it with the current face selected. Cards: Agents, the user's
faces, Settings, and "+" (hidden at eight faces), four at a time.

- SCROLL selects, or moves a grabbed card one slot.
- ENTER grabs or drops a movable card, goes to a pinned one, opens the gallery
  on "+".
- CANCEL reverts a grab, else asks to remove a removable card (ENTER removes,
  CANCEL keeps), else does nothing.
- Double-SHIFT or a tap exits to the selected face ("+" exits to where the
  manager opened). A double-SHIFT with a card grabbed reverts the grab first.
- Touch: tap = go, a quick horizontal drag scrolls the row, a 350 ms
  long-press grabs, then the card follows the finger slot by slot; Up drops,
  a cancelled touch reverts.
- Gallery: Omarchy, Macropad, Usage (a fourth Macropad is refused). Then
  naming: hold PTT and say a name (the host answers with `name`), ENTER accepts
  it or the template's name, CANCEL goes back. Double-SHIFT while naming
  cancels the add. A new face takes the lowest free id and goes in before
  Settings.
- One `faces` event per completed change (drop, remove, add); a reverted grab
  sends nothing. A host `faces` (strip restore) is applied only while the
  manager is closed.

## Notices

A notice is created by a new card (Question or Ready), keyed by agent and card.

- **Toast.** Slides down from the top for 4 s, counted from the frame that shows
  it; tapping it jumps to the card. So does ENTER, when the toast arrived with
  nothing it could interrupt (the auto-jump conditions below: nothing held, no
  PTT session, no overlay, no card on screen, the strip at rest, no input in
  the last 2 s) and ENTER is the first thing pressed, turned or touched since.
  Otherwise ENTER keeps its meaning on the face, and it always does on Omarchy
  and the Macropad, whose keys go to the computer, and as SHIFT+ENTER. The
  card then answers the next ENTER, not within 0.5 s of opening. Touches in its
  rect are swallowed for 300 ms after it expires. Message toasts ("no channel",
  "not delivered") last 2 s and belong to no notice.
- **Drawer.** A notice is filed when its toast times out or is displaced, when
  its card is snoozed, and when it arrives under an opaque overlay (drawer,
  manager, gallery, naming, LISTENING) or during a host resync; those get no
  toast. SCROLL
  selects, ENTER or a tap jumps to the card, LEFT/RIGHT or a horizontal swipe
  dismisses the notice (the card stays), CANCEL closes. At most eight; a full
  drawer drops the oldest Ready first.
- **Indicator.** A pill at the top centre while the drawer is non-empty: red if
  any Question waits, else green.
- A notice goes away when its card is answered or acknowledged, when the host
  clears or replaces the card, or when the agent is gone.
- **Auto-jump** (Settings → Alerts, off by default) jumps to a new card
  instead of toasting, unless something is held, a PTT session is live, an
  overlay is open, a card is on screen on the Agents face, the strip is sliding,
  or there was input in the last 2 s; then it toasts.

## PTT sessions

- The press latches the session's target: the card's agent (`answer`) on a
  card, the new face (`name`) while naming, the highlighted agent on the roster,
  else the active channel. With no target a "no channel" toast appears and
  nothing is sent. If the target is not the active channel, `channel` is sent
  first.
- The full-screen LISTENING overlay shows the agent, a waveform, the live
  transcript and the last reply (or the question being answered). It is drawn
  over every face.
- The release sends `ptt act:0` with the latched agent, context and card,
  whatever is on screen by then. CANCEL during the session aborts it
  (`abort:1`, the host discards the utterance). A session is released
  automatically after 60 s.
- While a session is live, navigation, slides, double-SHIFT, jumps and
  idle-home wait; a host `focus` applies after the release.
- Without a deck host (and outside the demo) the overlay shows "NO HOST",
  the waveform lies flat and nothing is sent; the release only ends the session.

## Idle-home and the dark screen

- Dim after 30 s without local input, panel off after 2 min. Host messages,
  system RPC requests and battery updates do not count as input.
- Panel sleep locks every local control, including PTT. Any button, wheel
  detent or touch lights the lock screen without acting. Four counterclockwise
  wheel ticks unlock it, with visible progress; clockwise rotation or another
  sleep resets progress. All four ticks are consumed, including the unlocking
  tick. A button or touch begun while locked stays suppressed until release,
  even if the wheel unlocks meanwhile. Controls queued before the wake frame
  reached the glass are also suppressed through release.
- A button trapped down while locked does not prevent dimming or another
  sleep. Cold power-on starts unlocked; ordinary dimming does not lock.
- A new card lights the screen for 15 s without unlocking or counting as input: the
  inactivity clock that idle-home reads keeps running. Demo notices never wake
  the screen.
- Idle-home runs on the same inactivity clock (0 while an unlocked control is held), after
  the Idle home setting (2 min by default, the same moment the panel sleeps): it
  closes overlays and the manager silently (no `faces`, no `snooze`), releases
  what is held, cuts to the Agents roster and arms the confirm guard. It is
  suspended during a PTT session and while naming.

## Power off

- Hold the CANCEL the deck itself owns: plain CANCEL on every face and overlay
  (SHIFT+CANCEL on Omarchy goes to the host and does not count). The
  press's own action fires first as always (home, back, close, keep, snooze,
  abort); the continued hold is the gesture.
- After 700 ms a non-opaque `Power off?` dialog opens over whatever is on
  screen, with an amber bar that fills until 3 s. Releasing before that closes
  it and does nothing else; any other button, a detent or a new touch cancels
  it too. SHIFT edges and the tail of a finger that was already down do not.
- A hold cannot start on a dark screen: that CANCEL press only wakes
  (`Idle-home and the dark screen`). A hold on a dim screen counts.
- At 3 s the deck releases every host-visible hold, aborts a PTT session,
  sends `up` as its last line, darkens the panel, cuts the panel rails and
  hands the chip to its PMU. Over USB the host sees exactly an unplug; over
  BLE a supervision timeout. Nothing is persisted.
- Power on: PTT, held for 2 s. The PMU wakes on PTT alone (CANCEL is no
  longer a wake pin, so a deck in a pocket stays off), and the firmware then
  insists on PTT staying down until 2 s after the application starts
  (`pmu::POWER_ON_HOLD_MS`); a shorter press boots the chip, which goes
  straight back to sleep with the panel never lit. A held PTT is a cold boot
  through the bootloader: splash, then the Agents roster (or the tutorial, on
  a deck that never finished it), with the strip and settings back at their
  defaults until the host restores them on `boot:1`. The press that woke it
  does nothing else; release it before starting a fresh PTT session.
- Rev A only. The dev board has no power path to switch, and the native
  image shows the dialog but stays on.

## Tutorial

A deck whose preferences say the tutorial was never finished opens it right
after the splash, over the Agents roster; Settings > Display > Tutorial
replays it. It is opaque and takes every control before the global layer
(the power-off hold still counts). Six steps, each waiting for the control
it teaches. The Buttons, Wheel, Latest and PTT steps show a front view of the deck
(`overlay::device`, drawn from the BodyV6.13 case model: wheel in the
top-left corner, PTT below it on the left edge, ENTER / CANCEL / SHIFT down
the right edge, LEFT and RIGHT under the display) with the control being
asked for lit green and labelled:

1. Buttons: ENTER, CANCEL, SHIFT, LEFT, RIGHT, one at a time in that order.
   The page reads "Press CANCEL", one line says what the key does, the key
   is lit on the drawing, and the right press moves to the next key ("That
   was ENTER · 2 of 5"). Any other button, PTT included, is only named
   ("That's SHIFT, not CANCEL", in amber). The fifth key ends the step.
2. Wheel: three detents, either way.
3. Panels: SHIFT held plus two detents; the strip's names show the selection
   move (the real strip never slides). A detent without SHIFT only changes
   the caption. A footnote mentions the double-SHIFT manager.
4. Latest: hold SHIFT and press ENTER. Both keys light on the drawing; the
   explanation teaches jumping to the latest chat message and resuming live
   following after scrolling. ENTER alone prompts the user to hold SHIFT.
5. PTT: press and hold (a LISTENING pill; no session starts), then let go.
6. Done: how to power on and off, and where to replay; ENTER closes it.

CANCEL on steps 2-5 skips to Done on a first run and closes a replay. Closing
from Done, or skipping, sets `Update::tutorial_done`, which the firmware
writes to its preference journal (`link::save_prefs`). SHIFT edges are
mirrored into the global modifier and button releases reach their owners, so
nothing is left held when it closes. Touch is ignored. Idle-home never closes
it; the screen still dims and sleeps on the usual timers. The native image
never opens it by itself (it has no preference store) but replays it from
Settings.

## Deck host and demo mode

- The host says `hello` on connect and every 5 s. The device answers with its
  firmware version, held session id, whether the strip and settings are still
  defaults (`boot`), and the strip. A new session resyncs: the old data stays
  drawn but stale until `synced`. 15 s without a hello means the host is gone:
  holds and a PTT session are torn down, the data turns gray and `offline`, and
  cards go inert. If something was held into that gap, the next `hello` reply
  is followed by `up`.
- The device sends nothing on channel 5 before it has received a channel-5 line
  on the current link.
- **Demo mode.** With Demo data on (off by default; Settings > Demo data turns
  it on) and no host yet, the deck
  shows the mockup session and a built-in host answers every event as a daemon
  would: an answer clears the card and sets the agent to Working, and later
  another agent asks something and the first one reports Ready; Omarchy keys
  cycle canned workspaces; Macropad keys flash; refresh nudges the bars; PTT
  types out a transcript and a reply; naming gets a canned name. The script
  only advances as a consequence of input, pauses while the screen is dark, and
  never wakes it. Header tag `demo`.
- The first real `hello` clears the demo. After a host has been seen, the demo
  never comes back by itself; a lost host's data stays on screen as `offline`.

## Known limitations

- **Nothing here has been validated on hardware.** Every rule above is covered
  by host tests, the native harness and rendered previews only. The
  [first-flash checklist](deck-device-checklist.md) is the plan.
- **No deck daemon exists yet.** `tools/deck-sim.py` is the only host: it runs
  the mockup session, a title flood and hand-written lines against a device.
- **The strip and the settings do not persist on the device.** They reset on
  every boot; the host is expected to store the `faces` and `set` it receives
  and restore them after `hello` (`boot:1` says the device is still on
  defaults). What the device does keep, in a flash journal, is its Bluetooth
  pairing and whether the tutorial was finished; Settings > About > Factory
  reset blanks both.
- **Speaker, mic gain, auto-announce and chime are host-side.** The device only
  stores and reports them. On the Rev A Embassy image the host applies them to
  the samples it sends over the BLE voice service (the DAC runs at a fixed
  level); on the native image audio levels are still set by macOS and the SDK.
- **The slide draws faces edge to edge.** The mockup shows a small gap between
  panels; the incoming face draws its own corner arcs instead.
- **The remove-confirm dialog lets the manager cards peek out** about 16 px on
  either side (there is no mockup for it).
- **One per-edge event may take two reports.** An aborted PTT release on a card
  is 67 bytes plus LF; every other per-edge event fits one report
  ([deck-protocol.md](deck-protocol.md)).
- **Font atlases depend on the FreeType build.** `tools/generate-ui-fonts.py
  --check` may report differences on a machine with another FreeType; commit
  regenerated atlases together with the change that needed them.
- **Render cost on the device is unmeasured** for the deck faces; see
  [display performance](display-performance.md#agent-command-deck-faces).
- **Power off wakes on PTT only, held 2 s.** The SF32LB52x PMU has two
  deep-sleep wake inputs and the firmware arms one, PTT; CANCEL, ENTER,
  SHIFT, the macro keys and the wheel do not power the deck on. The 2 s are
  counted from the application's start, so the ROM and bootloader add a few
  hundred milliseconds to the hold. There is no automatic power-off yet: a
  deck left dark on its cell runs the cell down.
