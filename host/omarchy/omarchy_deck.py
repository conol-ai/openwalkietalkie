#!/usr/bin/env python3
"""Omarchy (Hyprland) remote for the OpenWalkieTalkie deck (Python 3.10+, stdlib only).

Run on the Omarchy machine while a `deckd` is reachable through its Unix socket
(README.zh-CN.md has the whole setup):

    ssh -N -L "$XDG_RUNTIME_DIR/deckd.sock:/Users/you/Library/Application Support/OpenWalkieTalkie/deckd.sock" mac
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

EVENTS = ['device.ws', 'device.key', 'device.link', 'agent.order', 'focus.changed']
MAX_WINDOWS = 6
MAX_CHIPS = 9
TITLE_MAX = 48
SLOTS = 10  # Omarchy gives every monitor its own workspaces 1-10 (a block of ten)
REPUSH_S = 30.0
KIMI_POLL_S = 1.0
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


def socket_path() -> Path:
    if os.environ.get('DECKD_SOCKET'):
        return Path(os.environ['DECKD_SOCKET'])
    if os.environ.get('DECKD_DATA_DIR'):
        return Path(os.environ['DECKD_DATA_DIR']) / 'deckd.sock'
    return Path(os.environ.get('XDG_RUNTIME_DIR', Path.home() / '.local/state')) / 'deckd.sock'


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
        elif kind == 'device.link':
            self.last = None
        self.dirty.set()

    def serve(self, path: Path) -> None:
        self.rpc = Rpc(path)
        try:
            self.rpc.call('session.hello', {
                'client': {'name': 'Omarchy remote', 'version': '1'},
                'scopes': ['device', 'observe', 'agent'], 'subscribe': EVENTS, 'snapshot': False})
            self.up = True
            self.last = None
            self.bridge.reregister()  # first: sessions register whether or not Hyprland answers
            self.try_push(force=True)
            while True:
                message = self.rpc.receive(0.25)
                if message and message.get('method') == 'event':
                    self.handle(message['params'])
                while self.rpc.events:
                    self.handle(self.rpc.events.popleft())
                while not self.hooks.empty():
                    self.bridge.on_hook(self.hooks.get_nowait())
                now = time.monotonic()
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
    parser.add_argument('--socket', type=Path, default=socket_path(), help='deckd Unix socket')
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
            remote.serve(args.socket)
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
