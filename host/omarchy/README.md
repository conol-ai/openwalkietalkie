# Omarchy bridge

The deck as a remote for an Omarchy (Hyprland) laptop: windows and workspaces, the scratchpad, Claude Code / Codex / Kimi Code sessions, and voice orders. [简体中文](README.zh-CN.md)

**Linux with this bridge is a supported target.** For every pull request and every push to `main`, the CI job "Linux host" on Ubuntu 24.04 builds the host workspace for Linux and runs its tests (every crate but the Mac app: `deckd`, `deck-proto`, `deck-model`, `deck-client`, `deck-gui-core` and `deck-wrap`), then runs this directory's Python tests (see [Tests](#tests)). The window is macOS only; there is no Linux window yet. The bridge talks to `deckd` through the face feed (`device.ws.set` and the `device.ws`, `device.key` and `device.link` events), which is a public interface documented in the [native companion API](https://openwalkietalkie.org/docs/native-companions/#feed-the-omarchy-face).

**On Linux, `deckd` does not tell your processes apart.** Two checks of the macOS service are not implemented on Linux: the ancestry cap, which keeps a process started by an agent CLI from asking for more than `observe,agent,voice,hooks:ingest`, and the trusted principal, the signed app. So every process of your user, one started by Claude Code, Codex or Kimi Code included, may ask `deckd`'s socket for `admin` (its settings, integrations, setup checks and tokens), and no client is ever trusted, so an allow a client sends for a permission card is refused. A firmware install still needs a press on the deck, unless the deck is already in recovery.

**Recommended: the Linux `deckd` installed on Omarchy itself.** The deck pairs with Omarchy over Bluetooth. BlueZ exposes its HID service as `/dev/hidraw*`, which `hid.rs` drives; voice travels over the deck's audio GATT service (BlueZ) and is recognised on the machine by Whisper. Two systemd user services run on Omarchy: `deckd.service` (the Linux `deckd`) and `omarchy-deck.service` (the Python bridge in this directory).

```
deck ──Bluetooth── Omarchy ─┬─ deckd (Linux)
                            ├─ omarchy_deck.py   the bridge (Hyprland ⇄ deckd)
                            └─ omarchy_hook.py   the hook for Claude Code / Codex / Kimi Code
```

The deck can also stay with a Mac: see [The deck on a Mac](#the-deck-on-a-mac) below. Then no `deckd` runs on Omarchy.

```sh
# On Omarchy: build the Linux deckd, then install it as systemd user services
cd host && cargo build --release --target x86_64-unknown-linux-gnu -p deckd
sh omarchy/install.sh target/x86_64-unknown-linux-gnu/release/deckd
python3 omarchy/omarchy_hook.py install kimi     # Kimi Code; claude and codex the same way
```

The build needs `pkg-config`, the development files of libudev (hidapi), ALSA (cpal) and D-Bus (bluer), and CMake and libclang for whisper.cpp. The CI job installs them on Ubuntu as `pkg-config libudev-dev libasound2-dev libdbus-1-dev cmake libclang-dev` (`.github/workflows/ci.yml`).

`install.sh` copies the bridge to `~/.local/share/openwalkietalkie/omarchy/`, installs `deckd` to `~/.local/bin/deckd` when it is given the binary, and enables and starts the two units in `systemd/` (it is idempotent; `systemctl --user disable --now deckd omarchy-deck` undoes it). `deckd.service` runs `deckd run` with `DECKD_DATA_DIR=~/.local/share/openwalkietalkie`, so its config, socket and caches are all under that directory; `omarchy-deck.service` starts the bridge on that socket. (A `deckd` started without `DECKD_DATA_DIR` uses the XDG directories instead, and its socket is `$XDG_RUNTIME_DIR/openwalkietalkie/deckd.sock`, or `${XDG_CACHE_HOME:-~/.cache}/openwalkietalkie/deckd.sock` when `XDG_RUNTIME_DIR` is unset, which is where the bridge looks when it is started without `--socket`.) `omarchy_hook.py install` writes into each CLI's settings a hook command that points at the copy, and that command lets the call through when the script is missing, so moving the repository never blocks a session. The bridge finds the running Hyprland instance and Wayland display by itself, with no environment variables to set.

### Voice (PTT)

- **Recognition** is the local Whisper model. There is no local microphone: the deck's is the only input. Download a model before the first press. On a CPU, `large-v3-turbo` takes about 20 s per press; `small` (about 2 to 4 s) is the one to use:

```sh
echo '[voice.stt]
engine = "whisper"
model = "small"
language = "auto"' >> ~/.local/share/openwalkietalkie/config.toml
DECKD_DATA_DIR=~/.local/share/openwalkietalkie deckd stt install --model small
```

`stt install` downloads from Hugging Face. When that cannot be reached, fetch the same file from a mirror (`https://hf-mirror.com/ggerganov/whisper.cpp/resolve/main/ggml-small.bin`) into `~/.local/share/openwalkietalkie/caches/stt/`. Chinese comes out in Simplified characters: on Linux, speech recognised as Chinese is decoded again with a Simplified Chinese prompt.

- **Spoken replies** come from a speech program on the machine (the text on its stdin, a WAV on its stdout). The default is `espeak-ng` (`sudo pacman -S espeak-ng`); `[voice.tts] command` names another, such as piper. Without one, each announcement logs an error and nothing else is affected. The voice is the first of `[voice.tts] voices`; with none set, `cmn` or `en-us`, by whether the text contains Chinese.
- **Orders into a session**: the recognised text is typed into the selected session's terminal, followed by Enter. A session in a herdr pane gets it through `herdr pane run` (a pane that moved is found again by the session's pid); otherwise the bridge focuses the session's window and types with `wtype`.

### Kimi Code

Kimi Code has hooks (`[[hooks]]` in its `config.toml`). `omarchy_hook.py install kimi` writes one marked block, backing the file up first, and `uninstall` restores it as it was; it checks the result with `tomllib`, so it needs Python 3.11 or newer. Without the hooks, the bridge reads `~/.kimi-code/sessions/*/agents/main/wire.jsonl` for each session's state instead (`KIMI_CODE_HOME` is honoured). At the end of every reply, a summary of it goes to the deck.

## What it does

| Feature | How |
|---|---|
| **The Omarchy face** | Shows the focused monitor's workspaces (Omarchy gives each monitor its own 1 to 10) and up to 6 windows of the current workspace, the focused one highlighted |
| Switch windows | Wheel counter-clockwise / clockwise = next / previous window (`cycle_next`); tap a window row to focus it |
| Switch workspaces | Tap a workspace chip; LEFT / RIGHT = previous / next workspace (stepping through the fixed slots 1 to 10, empty ones included) |
| Move the focus | SHIFT+LEFT / SHIFT+RIGHT = focus the window to the left / right (SHIFT is the bottom key on the right, HOME: hold it, then press the direction) |
| Fullscreen | ENTER = toggle fullscreen |
| Scratchpad | SHIFT+CANCEL = show / hide the scratchpad; SHIFT+ENTER = move the current window to the scratchpad (Super+S and Super+Alt+S) |
| **Omarchy sessions on the roster** | Every Claude Code / Codex / Kimi Code session on Omarchy appears on the Agents roster, as working / ready / question / error |
| Select a session | Selecting a session on the deck brings its terminal window to the front on Omarchy |
| **Voice orders** | Press PTT on a selected Omarchy session and speak: the recognised text is typed into its terminal, followed by Enter (with `wtype`) |

The key map is `KEYMAP` at the top of `omarchy_deck.py`; that is the one place to change it.

## What it deliberately does not do

- **Permission prompts are not answered on the deck.** The dialog stays in the terminal. While a session is in the `question` state, voice orders to it are refused (`dialog_open`), so nothing you say can be taken as the answer to a prompt.
- **Little leaves Omarchy.** Only event names, states, project names and the notification line Claude itself shows (the question's title). Prompts and tool inputs stay on Omarchy; for Kimi Code, only a summary of each reply (600 characters at most) goes to the deck.
- **No pid or terminal is sent to `deckd`.** `deckd` checks a pid as a process on its own machine, so a session is registered with its name, its project directory and its capabilities only.
- **A hook never blocks.** Whether the bridge is there or not, and whatever the data, the hook prints `{}` and exits 0: it never stops your tool call.

## The deck on a Mac

The deck keeps one Bluetooth pairing at a time. While it is paired with a Mac, the bridge on Omarchy reaches the Mac's `deckd` through its Unix socket, forwarded over SSH. No `deckd` runs on Omarchy then, and there is no voice. Everything is in `host/omarchy/`, uses only the Python 3.10+ standard library, and needs no `pip install`.

### 1. On the Mac: a socket path SSH can forward

The socket's path has a space in it, which makes forwarding awkward, so give it a symlink:

```sh
ln -sf "$HOME/Library/Application Support/OpenWalkieTalkie/deckd.sock" "$HOME/.deckd.sock"
```

and turn on Remote Login in System Settings → General → Sharing → Remote Login.

### 2. On Omarchy: the forward

To try it by hand:

```sh
ssh -N -o ServerAliveInterval=15 -o ExitOnForwardFailure=yes \
    -L "$XDG_RUNTIME_DIR/deckd.sock:/Users/<you>/.deckd.sock" <you>@<mac-address>
```

To keep it, make it a systemd user service, `~/.config/systemd/user/deckd-tunnel.service`:

```ini
[Unit]
Description=deckd socket tunnel to the Mac
After=network-online.target

[Service]
ExecStartPre=/usr/bin/rm -f %t/deckd.sock
ExecStart=/usr/bin/ssh -N -o ServerAliveInterval=15 -o ExitOnForwardFailure=yes -o StreamLocalBindUnlink=yes -L %t/deckd.sock:/Users/<you>/.deckd.sock <you>@<mac-address>
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
```

and run `systemctl --user enable --now deckd-tunnel`.

### 3. Start the bridge

As Omarchy does it, add a line to `~/.config/hypr/autostart.lua`:

```lua
o.launch_on_start("python3 /path/to/host/omarchy/omarchy_deck.py")
```

Or run `python3 omarchy_deck.py` in a terminal first to watch its log. Without `--socket` it connects to `$DECKD_SOCKET`, else `$DECKD_DATA_DIR/deckd.sock`, else where a Linux `deckd` started without `DECKD_DATA_DIR` listens (see the `install.sh` paragraph above) — or, while no socket is there, to `$XDG_RUNTIME_DIR/deckd.sock` (`~/.local/state/deckd.sock` when `XDG_RUNTIME_DIR` is unset), the tunnel's end. It looks again before every connection and reconnects by itself: a Mac restart, a `deckd` restart or a dropped tunnel needs nothing from you.

### 4. Install the hooks (so sessions reach the roster)

```sh
python3 omarchy_hook.py status            # what is there now
python3 omarchy_hook.py install claude    # writes ~/.claude/settings.json (backed up first)
python3 omarchy_hook.py install codex     # writes ~/.codex/hooks.json (backed up first)
python3 omarchy_hook.py uninstall all     # undo at any time: removes only what was added
```

An install merges and never overwrites: your own hooks stay where they are, and installing again replaces ours rather than adding a second set. The backups are beside the file, named `*.omarchy-deck.<timestamp>.bak`. **A session that is already open does not load new hooks: start a new one.**

## Tests

CI runs them from the repository root, on Ubuntu 24.04's Python 3.12, after the Linux host's Rust tests:

```sh
python3 -m unittest discover -s host/omarchy -p 'test_*.py'
```

They cover turning Hyprland's data into the face's `ws` line, the key map, the process tree and window matching, registering and releasing sessions, reconnecting after the daemon goes away, every reason a voice order is refused, hooks letting calls through, install and uninstall, and an end-to-end run against a fake `deckd`. Three of them (the Kimi Code install) need `tomllib`, so on a Python older than 3.11 they fail with `ModuleNotFoundError: No module named 'tomllib'`. **They all run against a faked `/proc` and Hyprland: none of them has run on a real Omarchy.**

---

## Known limitations

- Permission prompts can be answered only in the terminal, never on the deck.
- A workspace shows at most 6 windows, and there are at most 9 workspace chips, so Omarchy's 10th workspace does not show on the deck.
- Sessions are found only through their hooks: one that was open before the hooks were installed does not appear until it is reopened.
- Only Kimi Code's reply summaries reach the deck; for Claude and Codex, only the state does, never the reply's text.
- Claude and Codex hooks installed by an older version need `install` once more to gain `PostToolUse`; without it, a session stays in `question` after an approval until its turn ends.
- While the tunnel is down, the Omarchy face and the roster keep their last state, and catch up when it is back.

## Not yet verified

See issue ZhangHanDong/openwalkietalkie#1: speech actually played on the deck, real Claude and Codex sessions, USB, and a real Mac.
