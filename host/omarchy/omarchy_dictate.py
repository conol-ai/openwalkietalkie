"""PTT on the Omarchy face as dictation: hold to talk, release to put the words at the cursor.

The deck addresses PTT to its active channel on every face but Agents. So the bridge registers
one agent of its own, Dictate, and makes it the active channel while the deck shows the Omarchy
face (`focus.report`, source `desktop`: nothing is raised), putting back the channel that was
active when the deck leaves it. A press there is then an order to Dictate: deckd records it,
offers it to the bridge for transcription (Superwhisper, `omarchy_transcribe`, when it is
installed; deckd's own recogniser otherwise), and the order that follows is typed at the cursor of
whatever window has the keyboard, with no Enter. CANCEL while PTT is held aborts the press on
the deck itself, so nothing is transcribed or typed.

Dictate also stays on the Agents roster: selecting it there and pressing PTT dictates the same way.
"""
from __future__ import annotations

import socket
import subprocess
import sys

NAME = 'Dictate'
HEARTBEAT_S = 10.0


def log(message: str) -> None:
    print(f'omarchy-deck: dictate: {message}', file=sys.stderr, flush=True)


class Dictation:
    def __init__(self, call, transcribe: bool = False, host: str | None = None, run=subprocess.run,
                 ensure=None):
        self.call = call
        self.transcribe = transcribe
        self.key = f'omarchy:{host or socket.gethostname()}:dictate'
        self.run = run
        self.ensure = ensure or (lambda: None)  # finds Hyprland and the Wayland display for wtype
        self.agent: str | None = None
        self.on_omarchy = False
        # The active channel the Omarchy face took over, put back when the deck leaves it.
        self.restore: str | None = None
        self.last_beat = 0.0

    # registration -----------------------------------------------------------------
    def start(self) -> None:
        """At every connection: register Dictate afresh, then follow the face the deck shows."""
        self.agent, self.on_omarchy, self.restore = None, False, None
        self.register()
        self.follow_face()

    def register(self) -> None:
        caps = {'answer': 'none', 'order': 'push', 'focus': False}
        if self.transcribe:
            caps['transcribe'] = 'push'
        params = {'kind': 'other', 'name': NAME, 'key': self.key, 'caps': caps, 'lease_ttl_s': 30}
        try:
            result = self.call('agent.register', params)
        except Exception as error:
            if getattr(error, 'code', None) != -32004:  # another connection still holds the key
                raise
            self.call('agent.claim', {'key': self.key})
            result = self.call('agent.register', params)
        self.agent = result['agent']

    def tick(self, now: float) -> None:
        if self.agent is None or now - self.last_beat < HEARTBEAT_S:
            return
        self.last_beat = now
        try:
            self.call('agent.heartbeat', {'agents': [self.agent]})
        except (ConnectionError, TimeoutError, OSError):
            return
        except Exception as error:  # a lapsed lease: register afresh
            log(f'heartbeat: {error}')
            self.register()

    # the face ---------------------------------------------------------------------
    def follow_face(self) -> None:
        """Make Dictate the active channel on the Omarchy face, and put the old one back after."""
        if self.agent is None:
            return
        state = self.call('state.get')
        snapshot = state.get('snapshot', state) if isinstance(state, dict) else {}
        device = snapshot.get('device') or {}
        faces = device.get('faces') or snapshot.get('faces') or []
        face = device.get('face')
        template = next((f.get('t') for f in faces if isinstance(f, dict) and f.get('f') == face), None)
        focused = (snapshot.get('focus') or {}).get('agent')
        if template == 'omarchy' and not self.on_omarchy:
            self.on_omarchy = True
            self.restore = focused if focused != self.agent else self.restore
            if focused != self.agent:
                self.call('focus.report', {'agent': self.agent, 'source': 'desktop'})
        elif template != 'omarchy' and self.on_omarchy:
            self.on_omarchy = False
            # Only put the old channel back if nothing else took the focus meanwhile.
            if focused == self.agent:
                agents = {a.get('id') for a in snapshot.get('agents', {}).values()} \
                    if isinstance(snapshot.get('agents'), dict) else set()
                back = self.restore if self.restore in agents else None
                self.call('focus.report', {'agent': back, 'source': 'desktop'})
            self.restore = None

    # the order --------------------------------------------------------------------
    def on_order(self, data: dict) -> bool:
        """An order to Dictate: the words go to the cursor. False for any other agent's order."""
        if self.agent is None or data.get('agent') != self.agent:
            return False
        text = ' '.join(str(data.get('text', '')).split())
        if not text:
            return True
        try:
            self.ensure()
            typed = self.run(['wtype', '--', text], capture_output=True, timeout=10)
            ok = typed.returncode == 0
        except (OSError, subprocess.SubprocessError):
            ok = False
        if not ok:
            log('wtype could not type the dictation')
            order = data.get('order')
            if isinstance(order, str):
                try:
                    self.call('agent.order.result', {'agent': self.agent, 'order': order, 'ok': False,
                                                     'reason': 'wtype_failed'})
                except Exception as error:
                    log(f'order result: {error}')
        return True
