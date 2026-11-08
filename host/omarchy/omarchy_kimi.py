"""Kimi Code sessions for the deck roster (stdlib only).

Kimi Code reports itself through hooks (`omarchy_hook.py install kimi`). This
watcher is the fallback for when they are not installed: it finds sessions the
way a person would: a running `kimi` process, the project directory it runs in, and
the session Kimi keeps under `~/.kimi-code/sessions/<project>/<session>/`.
The session's `agents/main/wire.jsonl` is an append-only event log; the last
significant event says what the session is doing:

  turn.prompt / turn.steer / agent.turn.started   -> working
  interaction.request (an approval dialog)        -> question
  interaction.resolved                            -> working
  turn.ended / prompt.completed / turn.cancel ... -> ready

`poll()` turns changes into the same messages `omarchy_hook.py` sends, so the
bridge treats a Kimi session like any other. Only the event types are read:
no prompt, reply or tool text is kept, and a question's title is a fixed line.
"""
from __future__ import annotations

import json
import os
from pathlib import Path

from omarchy_sessions import PROC, read_cmdline

TAIL_BYTES = 256 * 1024
PROGRAMS = {'kimi', 'kimi-code'}

WORKING = {'turn.prompt', 'turn.steer', 'agent.turn.started', 'interaction.resolved', 'prompt.steered'}
READY = {'turn.ended', 'agent.turn.ended', 'prompt.completed', 'prompt.aborted', 'turn.cancel',
         'turn.step.interrupted'}
QUESTION = {'interaction.request'}

EVENT_FOR_STATE = {
    'idle': 'SessionStart',
    'working': 'UserPromptSubmit',
    'question': 'PermissionRequest',
    'ready': 'Stop',
}
QUESTION_TITLE = 'Kimi is waiting for approval'


def state_from_wire(lines) -> str:
    """The session state after the last significant event in `lines`."""
    state = 'idle'
    for line in lines:
        try:
            kind = json.loads(line).get('type')
        except (ValueError, AttributeError):
            continue
        if kind in QUESTION:
            state = 'question'
        elif kind in WORKING:
            state = 'working'
        elif kind in READY:
            state = 'ready'
    return state


def read_tail(path: Path, limit: int = TAIL_BYTES) -> list[str]:
    try:
        with open(path, 'rb') as handle:
            size = handle.seek(0, os.SEEK_END)
            handle.seek(max(0, size - limit))
            data = handle.read()
    except OSError:
        return []
    lines = data.decode('utf-8', 'replace').splitlines()
    return lines[1:] if size > limit and lines else lines


def last_reply(wire: Path | None) -> str:
    """The text of the last assistant message of the latest turn, or ''.

    Read when a turn ends, to show a summary on the deck; a prompt marks the
    turn's start, so a turn without text does not repeat the one before it.
    """
    if wire is None:
        return ''
    for line in reversed(read_tail(wire)):
        try:
            event = json.loads(line)
        except ValueError:
            continue
        kind = event.get('type') if isinstance(event, dict) else None
        if kind in ('turn.prompt', 'turn.steer'):
            return ''
        if kind != 'agent.message.appended':
            continue
        message = (event.get('message') or {}).get('message') or {}
        if message.get('role') != 'assistant':
            continue
        words = ' '.join(b.get('text', '') for b in message.get('content') or []
                         if isinstance(b, dict) and b.get('type') == 'text').split()
        if words:
            return ' '.join(words)
    return ''


def find_wire(sid: str, home: str | None = None) -> Path | None:
    root = Path(home or os.environ.get('KIMI_CODE_HOME') or os.path.expanduser('~/.kimi-code')) / 'sessions'
    return next(iter(root.glob(f'*/{sid}/agents/main/wire.jsonl')), None)


def is_kimi(argv: list[str]) -> bool:
    return any(os.path.basename(a) in PROGRAMS for a in argv[:3])


def kimi_pids(proc: str = PROC, me: int | None = None) -> list[int]:
    """Running interactive Kimi processes, oldest first."""
    me = me if me is not None else os.getpid()
    found = []
    try:
        names = os.listdir(proc)
    except OSError:
        return []
    for name in names:
        if not name.isdigit() or int(name) == me:
            continue
        argv = read_cmdline(int(name), proc)
        if argv and is_kimi(argv) and not {'web', 'server', 'acp', 'rc', 'remote'} & set(argv[1:3]):
            found.append(int(name))
    return sorted(found)


def process_cwd(pid: int, proc: str = PROC) -> str:
    try:
        return os.readlink(f'{proc}/{pid}/cwd')
    except OSError:
        return ''


def hooks_installed(home: str | None = None) -> bool:
    """Whether omarchy_hook.py's block is in Kimi's config: then hooks, not this log reader, are the source."""
    path = Path(home or os.environ.get('KIMI_CODE_HOME') or os.path.expanduser('~/.kimi-code')) / 'config.toml'
    try:
        return 'omarchy_hook.py' in path.read_text()
    except OSError:
        return False


class KimiWatcher:
    def __init__(self, home: str | None = None, proc: str = PROC):
        self.root = Path(home or os.path.expanduser('~/.kimi-code')) / 'sessions'
        self.proc = proc
        self.bound: dict[int, str] = {}      # pid -> session id
        self.cwd_of: dict[str, str] = {}     # session id -> cwd
        self.state: dict[str, str] = {}      # session id -> last reported state
        self.seen: dict[str, tuple] = {}     # session id -> (mtime_ns, size) of the wire log

    def sessions_for(self, cwd: str) -> list[tuple[float, str, Path]]:
        """(wire mtime, session id, wire path) of this project's sessions, newest first."""
        out = []
        try:
            projects = list(self.root.iterdir())
        except OSError:
            return []
        for project in projects:
            for session in project.glob('session_*'):
                try:
                    meta = json.loads((session / 'state.json').read_text())
                except (OSError, ValueError):
                    continue
                if meta.get('archived') or meta.get('cwd') != cwd:
                    continue
                wire = session / 'agents' / 'main' / 'wire.jsonl'
                try:
                    out.append((wire.stat().st_mtime, session.name, wire))
                except OSError:
                    continue
        return sorted(out, reverse=True)

    def wire_path(self, sid: str) -> Path | None:
        for wire in self.root.glob(f'*/{sid}/agents/main/wire.jsonl'):
            return wire
        return None

    def poll(self) -> list[dict]:
        """Hook-shaped messages for everything that changed since the last poll."""
        messages = []
        live = kimi_pids(self.proc)
        for pid in list(self.bound):
            if pid not in live:
                sid = self.bound.pop(pid)
                messages.append(self.message(pid, sid, 'SessionEnd'))
                self.state.pop(sid, None)
                self.seen.pop(sid, None)
        for pid in live:
            if pid not in self.bound:
                cwd = process_cwd(pid, self.proc)
                taken = set(self.bound.values())
                pick = next((s for s in self.sessions_for(cwd) if s[1] not in taken), None) if cwd else None
                if pick is None:
                    continue
                self.bound[pid] = pick[1]
                self.cwd_of[pick[1]] = cwd
            sid = self.bound[pid]
            wire = self.wire_path(sid)
            if wire is None:
                continue
            try:
                stat = wire.stat()
            except OSError:
                continue
            stamp = (stat.st_mtime_ns, stat.st_size)
            if self.seen.get(sid) == stamp and sid in self.state:
                continue
            self.seen[sid] = stamp
            state = state_from_wire(read_tail(wire))
            if self.state.get(sid) != state:
                self.state[sid] = state
                messages.append(self.message(pid, sid, EVENT_FOR_STATE[state],
                                             QUESTION_TITLE if state == 'question' else None))
        return messages

    def message(self, pid: int, sid: str, event: str, note: str | None = None) -> dict:
        msg = {'cli': 'kimi', 'event': event, 'session_id': sid,
               'cwd': self.cwd_of.get(sid, ''), 'pid': pid}
        if note:
            msg['ntype'] = 'permission_prompt'
            msg['message'] = note
        return msg
