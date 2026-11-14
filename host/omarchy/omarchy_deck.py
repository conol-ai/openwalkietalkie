#!/usr/bin/env python3
"""Omarchy (Hyprland) remote for the OpenWalkieTalkie deck (Python 3.10+, stdlib only).

Run on the Omarchy machine while a `deckd` is reachable through its Unix socket
(README.md has the whole setup): a `deckd` on this machine is found where it
listens, and a Mac's through the SSH tunnel the README sets up, for example

    ssh -N -L "$XDG_RUNTIME_DIR/deckd.sock:/Users/you/.deckd.sock" mac
    python3 omarchy_deck.py

It keeps the deck's Omarchy face current (`device.ws.set`: the focused monitor's
workspace chips and up to six windows of the active workspace) and turns what
the face sends back (`device.key`, `device.ws`) into Hyprland dispatches, using
the same Lua-first, legacy-fallback calls as Omarchy's own scripts.

Deck controls on the Omarchy face (the face's own grammar, docs/agent-command-deck.md):
  wheel CCW / CW        next / previous window (Alt+Tab)
  LEFT / RIGHT          previous / next workspace
  SHIFT+LEFT / RIGHT    focus the window to the left / right
  ENTER                 toggle fullscreen
  HOME+CANCEL           open / close the scratchpad (Super+S); its windows are then listed
  HOME+ENTER            move the focused window to the scratchpad (Super+Alt+S)
  tap a workspace chip  go to that workspace slot (Omarchy's per-monitor 1-10)
  tap a window row      focus that window
Edit KEYMAP to change them; an unmapped key does nothing.

It also puts the Claude Code and Codex sessions of this machine on the deck's
Agents roster (reported by omarchy_hook.py, see omarchy_sessions.py): selecting
one on the deck raises its terminal, and a voice order is typed into it.

Like deckd, it goes quiet without a deck: after ten minutes in which deckd's
`device.link` has said no deck is linked, it stops feeding the face, stops
looking at sessions (Kimi Code's logs, the session processes) and sends deckd
nothing about them. The hooks that arrive meanwhile only move each session's
entry in the bridge's own table, so when the deck links it starts again and
shows every session as it is then.
"""
from __future__ import annotations

import argparse
from collections import deque
import json
import os
from pathlib import Path
import queue
import select
import socket
import subprocess
import sys
import threading
import time

from omarchy_hypr import Hypr, dispatch, ensure_instance, focus_slot, focus_window, hyprctl_json  # noqa: E402,F401

from omarchy_kimi import KimiWatcher, hooks_installed  # noqa: E402
from omarchy_sessions import Bridge, ControlServer, control_socket_path  # noqa: E402

EVENTS = ['device.ws', 'device.key', 'device.link', 'agent.order', 'focus.changed',
          'device.macro', 'device.macros', 'device.profile']
MAX_WINDOWS = 6
MAX_CHIPS = 9
TITLE_MAX = 48
SLOTS = 10  # Omarchy gives every monitor its own workspaces 1-10 (a block of ten)
REPUSH_S = 30.0
KIMI_POLL_S = 1.0
# deckd's own window (`deckd::dormancy`): with no deck linked for this long,
# nothing is collected from agent sessions. The link can drop for seconds at a time.
DORMANT_AFTER_S = 600.0
# The link sessions in which deckd counts a deck as there (`host_online`).
LINKED_SESSIONS = ('live', 'syncing')
# Hyprland events that can change what the face shows.
HYPR_EVENTS = (
    'workspace', 'workspacev2', 'createworkspace', 'createworkspacev2',
    'destroyworkspace', 'destroyworkspacev2', 'focusedmon', 'focusedmonv2',
    'activewindow', 'activewindowv2', 'openwindow', 'closewindow',
    'movewindow', 'movewindowv2', 'windowtitle', 'windowtitlev2', 'fullscreen',
)

LUA = 'lua'
LEGACY = 'legacy'
# (key, shift) -> (Lua expression, legacy dispatcher + args). Press only, except
# the wheel, which arrives as act 2 steps.
KEYMAP = {
    ('ccw', 0): ('hl.dsp.window.cycle_next()', ['cyclenext']),
    ('cw', 0): ('hl.dsp.window.cycle_next({ next = false })', ['cyclenext', 'prev']),
    ('left', 1): ('hl.dsp.focus({ direction = "l" })', ['movefocus', 'l']),
    ('right', 1): ('hl.dsp.focus({ direction = "r" })', ['movefocus', 'r']),
    ('enter', 0): ('hl.dsp.window.fullscreen({ mode = "fullscreen" })', ['fullscreen', '0']),
    # Omarchy's Super+S and Super+Alt+S.
    ('cancel', 1): ('hl.dsp.workspace.toggle_special("scratchpad")', ['togglespecialworkspace', 'scratchpad']),
    ('enter', 1): ('hl.dsp.window.move({ workspace = "special:scratchpad", follow = false })',
                   ['movetoworkspacesilent', 'special:scratchpad']),
}
# LEFT / RIGHT step through the monitor's workspace slots 1-10 (and wrap), as
# Super+<n> would: Hyprland's own `m+1` only visits workspaces that have windows.
WORKSPACE_STEP = {'left': -1, 'right': 1}

# Macropad keys (`MacroKey.keys`, deck_proto's KEYSTROKE_KEYS) as wtype names them. deckd types
# them itself only on macOS; on Linux it has no desktop, so the bridge types them. The modifiers
# go by where they sit on the keyboard: `cmd` is the key a Mac has there, Super (Omarchy's own
# modifier), and `opt` / `alt` is Alt.
MACRO_MODIFIERS = {'ctrl': 'ctrl', 'shift': 'shift', 'opt': 'alt', 'alt': 'alt', 'cmd': 'logo'}
MACRO_KEYSYMS = {
    'enter': 'Return', 'esc': 'Escape', 'tab': 'Tab', 'space': 'space', 'backspace': 'BackSpace',
    'delete': 'Delete', 'home': 'Home', 'end': 'End', 'pageup': 'Prior', 'pagedown': 'Next',
    'up': 'Up', 'down': 'Down', 'left': 'Left', 'right': 'Right', 'minus': 'minus', 'equal': 'equal',
    'lbracket': 'bracketleft', 'rbracket': 'bracketright', 'backslash': 'backslash',
    'semicolon': 'semicolon', 'quote': 'apostrophe', 'comma': 'comma', 'period': 'period',
    'slash': 'slash', 'grave': 'grave',
    **{c: c for c in 'abcdefghijklmnopqrstuvwxyz0123456789'},
    **{f'f{n}': f'F{n}' for n in range(1, 13)},
}


def wtype_args(combo: str) -> list[str] | None:
    """`cmd+shift+4` -> wtype's arguments (modifiers held around one key), else None: the same
    grammar as deckd's `keystroke_valid`, modifiers in any order and exactly one key."""
    held: list[str] = []
    key = None
    for part in str(combo).split('+'):
        if part in MACRO_MODIFIERS:
            if MACRO_MODIFIERS[part] not in held:
                held.append(MACRO_MODIFIERS[part])
        elif key is None and part in MACRO_KEYSYMS:
            key = MACRO_KEYSYMS[part]
        else:
            return None
    if key is None:
        return None
    args = ['wtype']
    for m in held:
        args += ['-M', m]
    args += ['-k', key]
    for m in reversed(held):
        args += ['-m', m]
    return args


def _absolute_env(name: str) -> Path | None:
    """`$name` as a path when it is an absolute one, as deckd reads its XDG variables."""
    value = os.environ.get(name, '')
    return Path(value) if value and os.path.isabs(value) else None


def daemon_socket_path() -> Path:
    """Where a Linux `deckd` started without `DECKD_DATA_DIR` listens (its
    `config::Paths::detect`): `$XDG_RUNTIME_DIR/openwalkietalkie/deckd.sock`,
    else under `$XDG_CACHE_HOME`, else under `~/.cache`."""
    base = _absolute_env('XDG_RUNTIME_DIR') or _absolute_env('XDG_CACHE_HOME') or Path.home() / '.cache'
    return base / 'openwalkietalkie' / 'deckd.sock'


def socket_path() -> Path:
    """`$DECKD_SOCKET`, else `$DECKD_DATA_DIR/deckd.sock`, else where deckd
    listens by default (`daemon_socket_path`). An SSH tunnel to a Mac set up
    by an earlier README ends at `$XDG_RUNTIME_DIR/deckd.sock`, which is where
    this bridge used to look: while that socket is there and deckd's own is
    not, it is the one."""
    if os.environ.get('DECKD_SOCKET'):
        return Path(os.environ['DECKD_SOCKET'])
    if os.environ.get('DECKD_DATA_DIR'):
        return Path(os.environ['DECKD_DATA_DIR']) / 'deckd.sock'
    own = daemon_socket_path()
    tunnel = Path(os.environ.get('XDG_RUNTIME_DIR') or Path.home() / '.local/state') / 'deckd.sock'
    if not own.exists() and tunnel.exists():
        return tunnel
    return own


# --- Hyprland ---------------------------------------------------------------

def slot_of(workspace_id: int) -> int:
    return (workspace_id - 1) % SLOTS + 1


def clip(text: str, limit: int = TITLE_MAX) -> str:
    text = ' '.join(text.split())
    return text if len(text) <= limit else text[:limit - 1] + '…'


def window_color(client: dict, focused: bool) -> str:
    if client.get('urgent'):
        return 'red'
    if focused:
        return 'blue'
    if client.get('initialClass') == 'org.omarchy.agent':
        return 'green'
    return 'gray'


def build_ws(monitors: list, workspaces: list, clients: list):
    """The face's `ws` line and the addresses its window rows stand for.

    Returns ({active, count, sel, windows}, [address, ...]) for the focused
    monitor, or None when Hyprland reports no focused monitor.
    """
    monitor = next((m for m in monitors if m.get('focused')), None)
    if monitor is None:
        return None
    active_id = monitor['activeWorkspace']['id']
    if active_id < 1:  # a special workspace (scratchpad) is on top
        active_id = next((w['id'] for w in workspaces
                          if w.get('monitorID') == monitor.get('id') and w['id'] >= 1), 1)
    base = active_id - slot_of(active_id)
    mine = [w for w in workspaces
            if w.get('monitorID') == monitor.get('id') and base < w['id'] <= base + SLOTS
            and w.get('windows', 0) > 0]
    active_slot = slot_of(active_id)
    count = min(MAX_CHIPS, max([slot_of(w['id']) for w in mine] + [active_slot]))
    active = active_slot if active_slot <= MAX_CHIPS else 0
    # With the scratchpad open its windows are the ones on screen and the ones to list.
    shown_id = (monitor.get('specialWorkspace') or {}).get('id') or active_id
    on_active = [c for c in clients
                 if c.get('mapped', True) and not c.get('hidden')
                 and c.get('workspace', {}).get('id') == shown_id]
    # Spatial order, so the rows read like the screen; focus history picks the highlight.
    on_active.sort(key=lambda c: (c.get('at', [0, 0])[0], c.get('at', [0, 0])[1], c['address']))
    focused_addr = next((c['address'] for c in sorted(on_active, key=lambda c: c.get('focusHistoryID', 9))),
                        None)
    shown = on_active[:MAX_WINDOWS]
    if focused_addr and focused_addr not in [c['address'] for c in shown]:
        shown = shown[:MAX_WINDOWS - 1] + [next(c for c in on_active if c['address'] == focused_addr)]
    windows, addresses, sel = [], [], 0
    for index, c in enumerate(shown):
        is_focused = c['address'] == focused_addr
        if is_focused:
            sel = index
        label = c.get('class') or c.get('initialClass') or '?'
        title = c.get('title') or ''
        windows.append({'tone': window_color(c, is_focused),
                        'title': clip(f'{label} — {title}' if title else label)})
        addresses.append(c['address'])
    return {'active': min(active, count), 'count': count, 'sel': sel, 'windows': windows}, addresses


# --- deckd ------------------------------------------------------------------

class RpcError(Exception):
    def __init__(self, error):
        self.code = error['code']
        detail = error.get('data')
        super().__init__(f"{error['message']} {detail}" if detail else error['message'])


class Rpc:
    """One ordered JSON-RPC connection; events that arrive while waiting are queued."""

    def __init__(self, path: Path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            self.sock.settimeout(5)
            self.sock.connect(str(path))
        except BaseException:
            self.sock.close()
            raise
        self.buffer = b''
        self.events: deque = deque()
        self.seq = 0

    def receive(self, timeout: float):
        if b'\n' not in self.buffer:
            if not select.select([self.sock], [], [], max(0, timeout))[0]:
                return None
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError('daemon disconnected')
            self.buffer += chunk
            if len(self.buffer) > 2 * 1024 * 1024:
                raise ConnectionError('oversized daemon frame')
            if b'\n' not in self.buffer:
                return None
        line, self.buffer = self.buffer.split(b'\n', 1)
        return json.loads(line)

    def call(self, method: str, params: dict | None = None):
        self.seq += 1
        request_id = self.seq
        self.sock.sendall((json.dumps({'jsonrpc': '2.0', 'id': request_id,
                                       'method': method, 'params': params or {}}) + '\n').encode())
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            message = self.receive(deadline - time.monotonic())
            if message is None:
                continue
            if message.get('method') == 'event':
                if len(self.events) >= 1024:
                    raise ConnectionError('event backlog exceeded')
                self.events.append(message['params'])
            elif message.get('id') == request_id:
                if 'error' in message:
                    raise RpcError(message['error'])
                return message['result']
        raise TimeoutError(method)

    def close(self):
        self.sock.close()


# What a slow or confused hyprctl raises: Hyprland's trouble, not deckd's, so it must
# neither end the daemon connection nor the bridge.
HYPR_ERROR_LOG_S = 30.0
HYPR_ERRORS = (RuntimeError, subprocess.SubprocessError, ValueError)


class Remote:
    def __init__(self):
        self.rpc: Rpc | None = None
        self.addresses: list[str] = []
        self.last: dict | None = None
        self.last_push = 0.0
        self.dirty = threading.Event()
        self.hooks: queue.Queue = queue.Queue()
        self.bridge = Bridge(self.call, Hypr())
        self.kimi = KimiWatcher()
        self.kimi_polled = 0.0
        self.last_hypr_error = -HYPR_ERROR_LOG_S
        self.up = False  # whether the daemon connection got as far as hello
        self.kimi_seeded = False
        # Since when deckd has said no deck is linked (monotonic); None while one is.
        self.away_since: float | None = None
        self.quiet = False  # whether the bridge has gone quiet (`tick_quiet`)
        # Macropad keys by face id, as deckd holds them (`snapshot.macros`); typed here only when
        # deckd is on Linux: a Mac's deckd, through the tunnel, types them on the Mac itself.
        self.macros: dict[int, list] = {}
        self.types_macros = False
        self.run = subprocess.run

    # deck away -------------------------------------------------------------
    def dormant(self, now: float) -> bool:
        """Ten minutes in which deckd has said no deck is linked."""
        return self.away_since is not None and now - self.away_since >= DORMANT_AFTER_S

    def on_link(self, data: dict, now: float) -> None:
        """`device.link`: a deck counted as there ends the wait, anything else starts it."""
        if data.get('host_online'):
            self.away_since = None
        elif self.away_since is None:
            self.away_since = now

    def link_at_hello(self, now: float) -> None:
        """Where the link stands as a connection starts, from `service.info`: the link's session,
        and whether deckd has already gone quiet itself (`collecting`), which makes the bridge
        quiet at once. A deckd that answers neither starts the ten minutes now."""
        try:
            info = self.rpc.call('service.info')
        except RpcError:
            info = {}
        self.types_macros = info.get('platform') == 'linux'
        self.load_macros()
        if info.get('session') in LINKED_SESSIONS:
            self.away_since = None
            return
        start = now - DORMANT_AFTER_S if info.get('collecting') is False else now
        self.away_since = start if self.away_since is None else min(self.away_since, start)

    def tick_quiet(self, now: float) -> bool:
        """Whether the bridge is quiet now. Going quiet and waking are each said once, and a
        wake catches the roster and the face up at once."""
        quiet = self.dormant(now)
        if quiet != self.quiet:
            self.quiet = quiet
            if quiet:
                # Not "for ten minutes": at the hello, a deckd that has gone quiet itself makes the
                # bridge quiet at once (`link_at_hello`), however long the bridge has been up.
                print('no deck linked: the face and the sessions wait for it', file=sys.stderr)
            else:
                print('the deck linked: feeding the face and the roster again', file=sys.stderr)
                self.last = None
                self.bridge.reregister()
                self.try_push(force=True)
        return quiet

    def take_hooks(self, quiet: bool = False) -> None:
        """Hand what the hooks reported to the bridge, in order. While quiet the bridge only notes
        it in its own table: nothing is read or sent until the wake (`Bridge.on_hook`)."""
        while True:
            try:
                message = self.hooks.get_nowait()
            except queue.Empty:
                return
            self.bridge.on_hook(message, quiet=quiet)

    def kimi_messages(self) -> list[dict]:
        """Kimi session messages from the log reader.

        Without hooks it is the source, polled for good. With them it only
        runs once, so a restart finds the Kimi already open: hooks speak only
        when something happens in a session, and would leave it off the deck
        until the next prompt.
        """
        if hooks_installed() and self.kimi_seeded:
            return []
        self.kimi_seeded = True
        return self.kimi.poll()

    def call(self, method: str, params: dict | None = None):
        if self.rpc is None:
            raise ConnectionError('deckd is not connected')
        return self.rpc.call(method, params)

    # Hyprland side ---------------------------------------------------------
    def snapshot(self):
        return build_ws(hyprctl_json('monitors'), hyprctl_json('workspaces'), hyprctl_json('clients'))

    def try_push(self, force: bool = False) -> None:
        """push(), surviving Hyprland being slow or not up; the next tick looks again."""
        try:
            self.push(force)
        except HYPR_ERRORS as error:
            if time.monotonic() - self.last_hypr_error > HYPR_ERROR_LOG_S:
                print(f'hyprland: {error}; will retry', file=sys.stderr)
                self.last_hypr_error = time.monotonic()
            self.last_push = time.monotonic()

    def push(self, force: bool = False) -> None:
        built = self.snapshot()
        if built is None:
            return
        line, self.addresses = built
        now = time.monotonic()
        if not force and line == self.last and now - self.last_push < REPUSH_S:
            return
        self.rpc.call('device.ws.set', line)
        self.last, self.last_push = line, now

    # Macropad -------------------------------------------------------------
    def load_macros(self) -> None:
        """Every Macropad face's keys, from deckd's snapshot; the events keep them current."""
        if not self.types_macros:
            self.macros = {}
            return
        try:
            state = self.rpc.call('state.get')
        except RpcError:
            return
        snapshot = state.get('snapshot', state) if isinstance(state, dict) else {}
        self.set_macros(snapshot.get('macros'))

    def set_macros(self, macros) -> None:
        self.macros = {}
        for f, layout in (macros or {}).items():
            keys = layout.get('keys') if isinstance(layout, dict) else None
            if isinstance(keys, list):
                self.macros[int(f)] = keys

    def on_macro(self, data: dict) -> None:
        """A Macropad key pressed (or a wheel step on one): type its keystroke. A release types
        nothing, as on the Mac: the keystroke is a down-up pair already."""
        if not self.types_macros or not data.get('act'):
            return
        try:
            key = self.macros.get(int(data.get('f')), [])[int(data.get('i'))]
        except (IndexError, TypeError, ValueError):
            return
        combo = key.get('keys') if isinstance(key, dict) else None
        if not combo:
            return  # an event-only key
        args = wtype_args(combo)
        if args is None:
            print(f'macropad: {combo!r} is not a keystroke wtype can type', file=sys.stderr)
            return
        try:
            ensure_instance()  # wtype needs WAYLAND_DISPLAY, which a service does not inherit
            done = self.run(args, capture_output=True, timeout=5)
        except (OSError, subprocess.SubprocessError) as error:
            print(f'macropad: wtype: {error}', file=sys.stderr)
            return
        if done.returncode != 0:
            print(f'macropad: wtype failed for {combo!r}', file=sys.stderr)

    # deck side -------------------------------------------------------------
    def on_key(self, data: dict) -> None:
        k, act, shift = data.get('k'), data.get('act'), int(bool(data.get('shift')))
        if act not in (1, 2):  # a press, or a wheel step; a release does nothing
            return
        action = KEYMAP.get((k, shift))
        try:
            if action:
                dispatch(*action)
            elif k in WORKSPACE_STEP and not shift:
                self.step_workspace(WORKSPACE_STEP[k])
        except HYPR_ERRORS as error:
            print(f'hyprland: {error}; key {k} dropped', file=sys.stderr)

    def step_workspace(self, step: int) -> None:
        monitor = next((m for m in hyprctl_json('monitors') if m.get('focused')), None)
        if monitor is None:
            return
        active = monitor['activeWorkspace']['id']
        if active < 1:  # a special workspace is on top: step from the slot underneath
            active = 1
        focus_slot((slot_of(active) - 1 + step) % SLOTS + 1)

    def on_ws(self, data: dict) -> None:
        if 'go' in data:
            focus_slot(int(data['go']))
        elif 'win' in data:
            index = int(data['win'])
            if 0 <= index < len(self.addresses):
                focus_window(self.addresses[index])

    def handle(self, event: dict) -> None:
        kind, data = event.get('type'), event.get('data') or {}
        if kind in ('agent.order', 'focus.changed'):
            self.bridge.on_event(event)
        elif kind == 'device.key':
            self.on_key(data)
        elif kind == 'device.ws':
            self.on_ws(data)
        elif kind == 'device.macro':
            self.on_macro(data)
        elif kind == 'device.macros':
            if self.types_macros and isinstance(data.get('keys'), list):
                self.macros[int(data['f'])] = data['keys']
        elif kind == 'device.profile':
            if self.types_macros:
                self.set_macros(data.get('macros'))
        elif kind == 'device.link':
            self.last = None
            self.on_link(data, time.monotonic())
        self.dirty.set()

    def serve(self, path: Path) -> None:
        self.rpc = Rpc(path)
        try:
            self.rpc.call('session.hello', {
                'client': {'name': 'Omarchy remote', 'version': '1'},
                'scopes': ['device', 'observe', 'agent'], 'subscribe': EVENTS, 'snapshot': False})
            self.up = True
            self.last = None
            self.quiet = False
            self.link_at_hello(time.monotonic())
            if not self.tick_quiet(time.monotonic()):
                self.bridge.reregister()  # first: sessions register whether or not Hyprland answers
                self.try_push(force=True)
            while True:
                message = self.rpc.receive(0.25)
                if message and message.get('method') == 'event':
                    self.handle(message['params'])
                while self.rpc.events:
                    self.handle(self.rpc.events.popleft())
                now = time.monotonic()
                # No deck for ten minutes: no face, no session looked at, until one links. The hooks
                # still move the bridge's own table, so the wake shows each session as it is.
                if self.tick_quiet(now):
                    self.take_hooks(quiet=True)
                    continue
                self.take_hooks()
                if now - self.kimi_polled >= KIMI_POLL_S:
                    self.kimi_polled = now
                    for message in self.kimi_messages():
                        self.bridge.on_hook(message)
                self.bridge.tick(now)
                if self.dirty.is_set():
                    self.dirty.clear()
                    time.sleep(0.05)  # let a burst of Hyprland events settle
                    self.try_push()
                elif time.monotonic() - self.last_push > REPUSH_S:
                    self.try_push(force=True)
        finally:
            rpc, self.rpc = self.rpc, None
            rpc.close()


def watch_hyprland(remote: Remote) -> None:
    """Mark the face stale on every Hyprland event that can change it."""
    while True:
        try:
            ensure_instance()
            path = (Path(os.environ['XDG_RUNTIME_DIR']) / 'hypr' /
                    os.environ['HYPRLAND_INSTANCE_SIGNATURE'] / '.socket2.sock')
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
                sock.connect(str(path))
                buffer = b''
                while True:
                    chunk = sock.recv(65536)
                    if not chunk:
                        break
                    buffer += chunk
                    *lines, buffer = buffer.split(b'\n')
                    if any(line.split(b'>>', 1)[0].decode(errors='ignore') in HYPR_EVENTS for line in lines):
                        remote.dirty.set()
        except (KeyError, OSError) as error:
            print(f'hyprland event socket: {error}; retrying', file=sys.stderr)
        time.sleep(2)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split('\n\n')[0])
    # No default here: without --socket the path is looked up again before
    # every connection (`socket_path`), so a tunnel or a deckd that comes up
    # after the bridge is still found.
    parser.add_argument('--socket', type=Path, default=None,
                        help='deckd Unix socket (default: $DECKD_SOCKET, $DECKD_DATA_DIR/deckd.sock, '
                             "else deckd's own: $XDG_RUNTIME_DIR/openwalkietalkie/deckd.sock)")
    parser.add_argument('--control-socket', type=Path, default=control_socket_path(),
                        help='where omarchy_hook.py reports Claude Code / Codex events')
    args = parser.parse_args()
    remote = Remote()
    control = ControlServer(args.control_socket, remote.hooks)
    try:
        control.bind()
    except OSError as error:
        print(f'control socket {args.control_socket}: {error}', file=sys.stderr)
        return 1
    control.start()
    threading.Thread(target=watch_hyprland, args=(remote,), daemon=True).start()
    delay = 1.0
    while True:
        try:
            remote.serve(args.socket or socket_path())
        except KeyboardInterrupt:
            return 0
        except Exception as error:  # the bridge outlives anything one connection throws
            if remote.up:  # it was connected: the back-off starts over
                delay = 1.0
            remote.up = False
            print(f'{error}; reconnecting in {delay:.0f}s', file=sys.stderr)
            time.sleep(delay)
            delay = min(delay * 2, 15.0)


if __name__ == '__main__':
    sys.exit(main())
