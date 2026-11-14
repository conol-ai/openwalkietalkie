"""Omarchy coding-agent sessions on the deck roster (stdlib only).

`omarchy_hook.py` runs as a Claude Code / Codex hook on the Omarchy machine and
reports each event to a Unix socket owned by `omarchy_deck.py`. This module is
the daemon's half:

  * `ControlServer`: the local, owner-only socket the hooks write to.
  * `Bridge`: one virtual agent per session on the deck's `deckd` (the roster
    row, its state, its release), the compositor window each session lives in,
    selecting a row on the deck to raise that window, and voice orders typed into it.

What it deliberately does not do:
  * send a Linux pid or terminal identity to `deckd` (a macOS daemon would read
    a pid as one of its own processes);
  * forward prompts, tool input or replies: only the event name, the state, the
    project name and, for a question, the notification line Claude itself shows;
  * answer permission prompts: those stay in the terminal, as `deck-protocol`
    shows them on the deck as informational cards only.
"""
from __future__ import annotations

import json
import os
import queue
import socket
import subprocess
import sys
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path

PROC = '/proc'
# Program names per CLI; Kimi Code's process is `kimi-code`, its launcher `kimi`.
CLI_NAMES = {'claude': ('claude',), 'codex': ('codex',), 'kimi': ('kimi', 'kimi-code')}
# The agent kinds deckd knows; any other CLI registers as 'other'.
DECK_KINDS = {'claude', 'codex'}
MAX_HOOK_LINE = 64 * 1024
MAX_ORDER = 2000
TITLE_MAX = 64
REPLY_MAX = 600  # a turn's closing reply, as the deck shows it
GC_EVERY_S = 15.0
HEARTBEAT_EVERY_S = 10.0
FOCUS_WAIT_S = 1.0

# Hook events and what they say about a session. None: nothing to show.
STATE_BY_EVENT = {
    'SessionStart': 'idle',
    'UserPromptSubmit': 'working',
    'PreToolUse': 'working',
    'PostToolUse': 'working',
    'PermissionRequest': 'question',
    'PermissionResult': 'working',
    'Stop': 'ready',
    'StopFailure': 'error',
    'Interrupt': 'ready',
}
# Claude's Notification carries a type: a permission or elicitation dialog is a
# question, an idle prompt is the session waiting for its user.
STATE_BY_NOTIFICATION = {
    'permission_prompt': 'question',
    'elicitation_dialog': 'question',
    'idle_prompt': 'ready',
}


def control_socket_path() -> Path:
    base = os.environ.get('XDG_RUNTIME_DIR') or f'/tmp/omarchy-deck-{os.getuid()}'
    return Path(base) / 'omarchy-deck.sock'


def log(message: str) -> None:
    print(f'omarchy-deck: {message}', file=sys.stderr, flush=True)


# --- processes --------------------------------------------------------------

def read_stat(pid: int, proc: str = PROC):
    """(comm, ppid) of a process, or None when it is gone or unreadable."""
    try:
        text = Path(f'{proc}/{pid}/stat').read_text()
        comm = text[text.index('(') + 1:text.rindex(')')]
        return comm, int(text[text.rindex(')') + 2:].split()[1])
    except (OSError, ValueError, IndexError):
        return None


def read_cmdline(pid: int, proc: str = PROC) -> list[str]:
    try:
        raw = Path(f'{proc}/{pid}/cmdline').read_bytes()
    except OSError:
        return []
    return [part.decode(errors='replace') for part in raw.split(b'\0') if part]


def ancestors(pid: int, proc: str = PROC, limit: int = 40) -> list[int]:
    """`pid` first, then its parents up to init."""
    chain: list[int] = []
    while pid > 1 and pid not in chain and len(chain) < limit:
        chain.append(pid)
        stat = read_stat(pid, proc)
        if stat is None:
            break
        pid = stat[1]
    return chain


def cli_process(start_pid: int, cli: str, proc: str = PROC) -> int | None:
    """The nearest ancestor (or self) that is the `cli` program.

    `claude` is a Node/Bun program on some installs, so `node /…/claude` counts:
    the program name may be either of the first two arguments.
    """
    names = CLI_NAMES.get(cli)
    if names is None:
        return None
    for pid in ancestors(start_pid, proc):
        stat = read_stat(pid, proc)
        argv = read_cmdline(pid, proc)
        if (stat and stat[0] in names) or any(os.path.basename(a) in names for a in argv[:2]):
            return pid
    return None


HERDR_ENV_KEYS = ('HERDR_SOCKET_PATH', 'XDG_CONFIG_HOME', 'XDG_STATE_HOME', 'HOME', 'PATH')


def herdr_target(pid: int, proc: str = PROC):
    """(herdr binary, pane id, environment) when `pid` runs in a herdr pane, else None.

    herdr keeps its terminals in a background server, so no window is an
    ancestor of such a session; herdr itself is the way in.
    """
    try:
        raw = Path(f'{proc}/{pid}/environ').read_bytes().split(b'\0')
    except OSError:
        return None
    env = dict(item.decode('utf-8', 'replace').split('=', 1) for item in raw if b'=' in item)
    binary, pane = env.get('HERDR_BIN_PATH') or env.get('HERDR_BIN'), env.get('HERDR_PANE_ID')
    if env.get('HERDR_ENV') != '1' or not binary or not pane:
        return None
    return binary, pane, {k: env[k] for k in HERDR_ENV_KEYS if k in env}


def pid_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def window_for(cli_pid: int, clients: list, cwd: str = '', proc: str = PROC):
    """The Hyprland window a session runs in: the client whose pid is an ancestor.

    A terminal that serves several windows from one process (single-instance
    kitty, a shared foot server) leaves more than one candidate: the title that
    names the project wins, then the most recently focused.
    """
    by_pid: dict = {}
    for client in clients:
        by_pid.setdefault(client.get('pid'), []).append(client)
    base = os.path.basename(cwd.rstrip('/')) if cwd else ''
    for pid in ancestors(cli_pid, proc):
        candidates = by_pid.get(pid)
        if not candidates:
            continue
        if len(candidates) == 1:
            return candidates[0]
        named = [c for c in candidates if base and base in (c.get('title') or '')]
        pool = named or candidates
        return min(pool, key=lambda c: c.get('focusHistoryID', 1 << 30))
    return None


# --- sessions ---------------------------------------------------------------

@dataclass
class Session:
    key: str
    cli: str
    session_id: str
    cwd: str
    pid: int
    name: str
    state: str = 'idle'
    title: str = ''
    agent: str | None = None
    gone: bool = False
    synced: dict = field(default_factory=dict)
    reply: str = ''
    replied: str = ''
    # A Kimi turn ended while the bridge was quiet: its reply is read at the wake (`reregister`).
    reply_due: bool = False


def clip(text: str, limit: int) -> str:
    text = ' '.join(str(text).split())
    return text if len(text) <= limit else text[:limit - 1] + '…'


def state_for(event: str, notification_type: str | None) -> str | None:
    if event == 'Notification':
        return STATE_BY_NOTIFICATION.get(notification_type or '')
    return STATE_BY_EVENT.get(event)


def valid_hook(msg) -> bool:
    return (isinstance(msg, dict)
            and msg.get('cli') in CLI_NAMES
            and isinstance(msg.get('event'), str) and 0 < len(msg['event']) <= 64
            and isinstance(msg.get('session_id'), str) and 0 < len(msg['session_id']) <= 256
            and isinstance(msg.get('cwd', ''), str) and len(msg.get('cwd', '')) <= 4096
            and isinstance(msg.get('reply', ''), str) and len(msg.get('reply', '')) <= 8000
            and isinstance(msg.get('pid'), int) and msg['pid'] > 1)


def sanitize_order(text: str) -> str:
    """One line of printable text: a newline typed into a prompt would submit early."""
    text = ''.join(ch if ch.isprintable() else ' ' for ch in str(text))
    return ' '.join(text.split())[:MAX_ORDER]


class Bridge:
    """Sessions <-> virtual agents on the deck host.

    `call(method, params)` talks to `deckd` and raises ConnectionError /
    TimeoutError / OSError when the daemon is away, or an exception with a
    `code` attribute for a refusal; a session whose sync failed stays dirty and
    is retried by `flush`.
    """

    def __init__(self, call, hypr, host: str | None = None, run=subprocess.run,
                 sleep=time.sleep, proc: str = PROC):
        self.call = call
        self.hypr = hypr
        self.host = host or socket.gethostname()
        self.run = run
        self.sleep = sleep
        self.proc = proc
        self.sessions: dict[str, Session] = {}
        self.dirty: set[str] = set()
        self.last_gc = 0.0
        self.last_beat = 0.0
        # Whether the daemon takes `agent.order.result`; an older one answers
        # -32601 once and is not asked again.
        self.order_results = True

    # hooks -------------------------------------------------------------------
    def on_hook(self, msg: dict, quiet: bool = False) -> None:
        """One hook report. While the bridge is quiet (no deck for ten minutes) it still moves the
        session's entry here, so the wake shows every session as it is now, but nothing is read
        from the session's files and nothing goes to deckd until then (`reregister`)."""
        if not valid_hook(msg):
            return
        cli, sid = msg['cli'], msg['session_id']
        key = f'omarchy:{self.host}:{cli}:{sid}'
        cwd = msg.get('cwd', '')
        session = self.sessions.get(key)
        if session is None:
            if msg['event'] == 'SessionEnd':
                return
            name = clip(os.path.basename(cwd.rstrip('/')) or cli, 24)
            if cli == 'kimi':  # no Kimi icon on the deck: the name says which program it is
                name = clip(f'Kimi: {name}', 24)
            session = self.sessions[key] = Session(key, cli, sid, cwd, msg['pid'], name)
        session.pid, session.cwd = msg['pid'], cwd or session.cwd
        if msg['event'] == 'SessionEnd':
            session.gone = True
        else:
            if msg['event'] == 'Stop' and cli == 'kimi':
                if quiet:
                    session.reply_due = True
                else:
                    self.read_reply(session)
            elif msg['event'] == 'Stop' and msg.get('reply', '').strip():
                # Claude Code and Codex carry the reply in the hook itself (`last_assistant_message`).
                session.reply = clip(msg['reply'], REPLY_MAX)
            state = state_for(msg['event'], msg.get('ntype'))
            if state:
                session.state = state
                session.title = clip(msg.get('message') or '', TITLE_MAX) if state == 'question' else ''
        self.dirty.add(key)
        if not quiet:
            self.flush()

    def read_reply(self, s: Session) -> None:
        """A Kimi turn's closing reply, from its log."""
        from omarchy_kimi import find_wire, last_reply  # omarchy_kimi imports this module
        s.reply, s.reply_due = clip(last_reply(find_wire(s.session_id)), REPLY_MAX), False

    def flush(self) -> None:
        for key in list(self.dirty):
            session = self.sessions.get(key)
            if session is None:
                self.dirty.discard(key)
                continue
            try:
                self.sync(session)
            except (ConnectionError, TimeoutError, OSError):
                return  # the daemon is away; the rest wait with it
            except Exception as error:  # a refusal for this session only
                log(f'{session.name}: {error}')
                self.dirty.discard(key)
                if not session.gone:
                    session.agent, session.synced, session.replied = None, {}, ''  # register afresh on the next beat

    def sync(self, s: Session) -> None:
        if s.gone:
            if s.agent:
                try:
                    self.call('agent.release', {'agent': s.agent, 'reason': 'session_end'})
                except (ConnectionError, TimeoutError, OSError):
                    raise
                except Exception as error:  # the daemon no longer knows it: nothing to release
                    log(f'{s.name}: release: {error}')
            self.sessions.pop(s.key, None)
            self.dirty.discard(s.key)
            return
        if s.agent is None:
            self.register(s)
        # deckd speaks the stored reply the moment the state turns ready, so a turn's end sends the
        # reply first. A session registered just now (first seen, a reconnect, a refusal) is restored
        # state first instead: its reply is one it already had, and is not announced again.
        reply_first = bool(s.synced)
        if reply_first:
            self.push_reply(s)
        want = {'state': s.state, 'title': s.title}
        if s.synced != want:
            self.call('agent.update', {'agent': s.agent, **want})
            s.synced = want
        if not reply_first:
            self.push_reply(s)
        self.dirty.discard(s.key)

    def push_reply(self, s: Session) -> None:
        if s.reply and s.reply != s.replied:
            self.call('agent.reply', {'agent': s.agent, 'text': s.reply, 'speak': False})
            s.replied = s.reply

    def register(self, s: Session) -> None:
        params = {'kind': s.cli if s.cli in DECK_KINDS else 'other', 'name': s.name, 'key': s.key, 'cwd': s.cwd,
                  'caps': {'answer': 'none', 'order': 'push', 'focus': True}, 'lease_ttl_s': 30}
        try:
            result = self.call('agent.register', params)
        except Exception as error:
            if getattr(error, 'code', None) != -32004:
                raise
            self.call('agent.claim', {'key': s.key})
            result = self.call('agent.register', params)
        s.agent = result['agent']
        s.synced, s.replied = {}, ''

    def reregister(self) -> None:
        """Register every session afresh, as it is now: after a reconnect the daemon has forgotten
        this connection's agents, and at the wake from quiet it still shows them as they were when
        the bridge went quiet. A session that ended meanwhile keeps its agent, so it is released (a
        daemon that no longer knows it refuses, and the session is dropped all the same), and a
        Kimi turn that ended meanwhile has its reply read now."""
        for key, s in self.sessions.items():
            if s.reply_due:
                if s.state == 'ready' and not s.gone:
                    self.read_reply(s)
                s.reply_due = False
            if not s.gone:
                s.agent, s.synced, s.replied = None, {}, ''
            self.dirty.add(key)
        self.flush()

    # housekeeping ------------------------------------------------------------
    def tick(self, now: float) -> None:
        if now - self.last_gc >= GC_EVERY_S:
            self.last_gc = now
            for s in list(self.sessions.values()):
                if not pid_alive(s.pid):
                    s.gone = True
                    self.dirty.add(s.key)
        if now - self.last_beat >= HEARTBEAT_EVERY_S:
            self.last_beat = now
            for s in self.sessions.values():
                if s.agent is None and not s.gone:  # a registration that was refused: try again
                    self.dirty.add(s.key)
            agents = [s.agent for s in self.sessions.values() if s.agent and not s.gone]
            if agents:
                try:
                    self.call('agent.heartbeat', {'agents': agents})
                except (ConnectionError, TimeoutError, OSError):
                    return
                except Exception as error:  # a lapsed lease: the daemon dropped them, register afresh
                    log(f'heartbeat: {error}')
                    for s in self.sessions.values():
                        if s.agent and not s.gone:
                            s.agent, s.synced, s.replied = None, {}, ''
                            self.dirty.add(s.key)
        self.flush()

    # deck -> Omarchy ---------------------------------------------------------
    def by_agent(self, agent: str | None) -> Session | None:
        return next((s for s in self.sessions.values() if agent and s.agent == agent), None)

    def on_event(self, event: dict) -> None:
        kind, data = event.get('type'), event.get('data') or {}
        session = self.by_agent(data.get('agent'))
        if session is None:
            return
        if kind == 'agent.order' and str(data.get('text', '')).strip():
            why = self.deliver_order(session, data['text'])
            if why:
                log(f'{session.name}: order not delivered ({why})')
                self.report_failed_order(session, data.get('order'), why)
        # `device`: a session selected on the deck. `api`: `op.focus`, which deckd leaves to the app
        # that registered the session (it raises only a hook session's app itself). `desktop` is a
        # tracker reporting what is already in front: nothing to raise.
        elif kind == 'focus.changed' and data.get('source') in ('device', 'api'):
            window = window_for(session.pid, self.hypr.clients(), session.cwd, self.proc)
            if window:
                self.hypr.focus(window['address'])

    def report_failed_order(self, s: Session, order, why: str) -> None:
        """Tell deckd an order it handed us was not delivered, so the deck shows it.

        Only an order that came with an id can be reported (older daemons send
        none), and only to a daemon that has the method.
        """
        if not isinstance(order, str) or not self.order_results:
            return
        try:
            self.call('agent.order.result', {'agent': s.agent, 'order': order, 'ok': False, 'reason': why})
        except Exception as error:  # the daemon away, or a refusal: the log has it
            if getattr(error, 'code', None) == -32601:
                self.order_results = False
            else:
                log(f'{s.name}: order result: {error}')

    def herdr_pane_of(self, binary: str, env: dict, pid: int):
        """The herdr pane whose processes include `pid` now, else None."""
        def ask(*args):
            done = self.run([binary, *args], capture_output=True, text=True, timeout=5, env=env)
            return json.loads(done.stdout)['result'] if done.returncode == 0 else {}
        try:
            for pane in ask('pane', 'list').get('panes', []):
                info = ask('pane', 'process-info', '--pane', pane['pane_id']).get('process_info', {})
                pids = [p.get('pid') for p in info.get('foreground_processes', [])]
                if pid in pids or info.get('shell_pid') == pid:
                    return pane['pane_id']
        except (ValueError, KeyError, TypeError, subprocess.SubprocessError, OSError):
            pass
        return None

    def deliver_order(self, s: Session, text: str) -> str | None:
        """Type a voice order into the session's terminal; None on success, else why not.

        Guards, in the order they cost nothing: a question dialog is open (the
        text would answer it), the session ended, the window is not where the
        process says it is. The text is typed only after Hyprland confirms the
        window has focus, and checked again right before the Return.
        """
        text = sanitize_order(text)
        if not text:
            return 'empty'
        if s.state == 'question':
            return 'dialog_open'
        if not pid_alive(s.pid):
            return 'session_gone'
        target = herdr_target(s.pid, self.proc)
        if target:
            binary, pane, env = target
            sent = self.run([binary, 'pane', 'run', pane, text], capture_output=True, timeout=5, env=env)
            if sent.returncode != 0:
                # The pane id was fixed when the session started; panes get moved.
                found = self.herdr_pane_of(binary, env, s.pid)
                if found and found != pane:
                    sent = self.run([binary, 'pane', 'run', found, text], capture_output=True, timeout=5, env=env)
            return None if sent.returncode == 0 else 'herdr_failed'
        window = window_for(s.pid, self.hypr.clients(), s.cwd, self.proc)
        if window is None:
            return 'no_window'
        address = window['address']
        self.hypr.focus(address)
        waited = 0.0
        while self.hypr.active_address() != address:
            if waited >= FOCUS_WAIT_S:
                return 'focus_failed'
            self.sleep(0.05)
            waited += 0.05
        if not pid_alive(s.pid) or s.state == 'question' or self.hypr.active_address() != address:
            return 'changed'
        typed = self.run(['wtype', '--', text], capture_output=True, timeout=5)
        if typed.returncode != 0:
            return 'wtype_failed'
        self.sleep(0.1)
        pressed = self.run(['wtype', '-k', 'Return'], capture_output=True, timeout=5)
        return None if pressed.returncode == 0 else 'wtype_failed'


# --- the hooks' door --------------------------------------------------------

class ControlServer(threading.Thread):
    """Owner-only Unix socket: one JSON line per hook event, no reply needed."""

    def __init__(self, path: Path, events: queue.Queue):
        super().__init__(daemon=True, name='omarchy-control')
        self.path = path
        self.events = events
        self.listener: socket.socket | None = None

    def bind(self) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        try:
            self.path.unlink()
        except FileNotFoundError:
            pass
        listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        listener.bind(str(self.path))
        os.chmod(self.path, 0o600)
        listener.listen(16)
        self.listener = listener

    def same_user(self, conn: socket.socket) -> bool:
        option = getattr(socket, 'SO_PEERCRED', None)
        if option is None:  # the 0600 socket in the runtime dir is the guard there
            return True
        import struct
        creds = conn.getsockopt(socket.SOL_SOCKET, option, struct.calcsize('3i'))
        return struct.unpack('3i', creds)[1] == os.getuid()

    def run(self) -> None:
        assert self.listener is not None
        while True:
            try:
                conn, _ = self.listener.accept()
            except OSError:
                return
            with conn:
                try:
                    conn.settimeout(1.0)
                    if not self.same_user(conn):
                        continue
                    data = b''
                    while b'\n' not in data and len(data) <= MAX_HOOK_LINE:
                        chunk = conn.recv(4096)
                        if not chunk:
                            break
                        data += chunk
                    if len(data) <= MAX_HOOK_LINE:
                        self.events.put(json.loads(data.split(b'\n', 1)[0]))
                except (OSError, ValueError):
                    continue
