#!/usr/bin/env python3
"""Claude Code / Codex / Kimi Code hook for the Omarchy deck adapter (Python 3.10+, stdlib only).

    omarchy_hook.py hook claude|codex|kimi     what the CLIs run for every hook event
    omarchy_hook.py install [claude|codex|kimi|all]
    omarchy_hook.py uninstall [claude|codex|kimi|all]
    omarchy_hook.py status

`hook` is fail-open by construction: whatever goes wrong (no daemon, a bad
payload, a closed socket) it prints `{}` and exits 0, because a non-zero hook
exit is a *deny* for Claude Code and Codex. It reports only the event name, the
session id, the project directory and, for a question, Claude's own
notification line; never a prompt, a tool input or a reply.

`install` merges one handler per event into the CLI's own settings, keeping
everything already there, and backs the file up first. Codex hooks must also be
trusted in Codex (README.zh-CN.md). Kimi Code keeps its hooks as `[[hooks]]`
tables in `config.toml`, so ours live in one marked block there.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import shlex
import re
import shutil
import socket
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from omarchy_sessions import CLI_NAMES, cli_process, control_socket_path  # noqa: E402

MARKER = 'omarchy_hook.py'
STDIN_MAX = 1024 * 1024
CONNECT_TIMEOUT = 0.3
EVENTS = {
    'claude': [('SessionStart', 5), ('UserPromptSubmit', 5), ('PreToolUse', 5), ('PermissionRequest', 5),
               ('Notification', 5), ('Stop', 5), ('StopFailure', 5), ('SessionEnd', 5)],
    'codex': [('SessionStart', 5), ('UserPromptSubmit', 5), ('PreToolUse', 5), ('PermissionRequest', 5),
              ('Stop', 5), ('Interrupt', 5), ('SessionEnd', 5)],
    'kimi': [('SessionStart', 5), ('UserPromptSubmit', 5), ('PermissionRequest', 5), ('PermissionResult', 5),
             ('Stop', 5), ('StopFailure', 5), ('Interrupt', 5), ('SessionEnd', 5)],
}
KIMI_BEGIN = '# >>> omarchy-deck hooks (managed by omarchy_hook.py) >>>'
KIMI_END = '# <<< omarchy-deck hooks <<<'


def settings_path(cli: str, home: Path | None = None) -> Path:
    home = home or Path.home()
    if cli == 'claude':
        return Path(os.environ.get('CLAUDE_CONFIG_DIR') or home / '.claude') / 'settings.json'
    if cli == 'kimi':
        return Path(os.environ.get('KIMI_CODE_HOME') or home / '.kimi-code') / 'config.toml'
    return Path(os.environ.get('CODEX_HOME') or home / '.codex') / 'hooks.json'


def hook_command(cli: str) -> str:
    return f'{shlex.quote(sys.executable)} {shlex.quote(str(Path(__file__).resolve()))} hook {cli}'


# --- the hook ---------------------------------------------------------------

def build_message(cli: str, payload: dict, start_pid: int, proc: str = '/proc') -> dict | None:
    pid = cli_process(start_pid, cli, proc)
    session_id = payload.get('session_id')
    event = payload.get('hook_event_name')
    if pid is None or not isinstance(session_id, str) or not isinstance(event, str):
        return None
    message = {'cli': cli, 'event': event, 'session_id': session_id,
               'cwd': payload.get('cwd') if isinstance(payload.get('cwd'), str) else '', 'pid': pid}
    if event == 'Notification':
        message['ntype'] = payload.get('notification_type')
        message['message'] = str(payload.get('message') or '')[:200]
    return message


def send(message: dict, path: Path) -> bool:
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.settimeout(CONNECT_TIMEOUT)
            sock.connect(str(path))
            sock.sendall(json.dumps(message).encode() + b'\n')
        return True
    except OSError:
        return False


def run_hook(cli: str) -> int:
    try:
        raw = sys.stdin.buffer.read(STDIN_MAX + 1)
        payload = json.loads(raw) if len(raw) <= STDIN_MAX else {}
        message = build_message(cli, payload if isinstance(payload, dict) else {}, os.getppid())
        if message:
            send(message, control_socket_path())
    except Exception:  # fail open: never deny a tool call over a bug of ours
        pass
    print('{}')
    return 0


# --- install ----------------------------------------------------------------

def is_ours(group) -> bool:
    return isinstance(group, dict) and any(
        isinstance(h, dict) and MARKER in str(h.get('command', '')) for h in group.get('hooks', []))


def merge(settings: dict, cli: str, command: str) -> dict:
    """Our handler once per event; foreign groups untouched and in place."""
    hooks = settings.setdefault('hooks', {})
    if not isinstance(hooks, dict):
        raise ValueError('the existing `hooks` value must be an object')
    for event, timeout in EVENTS[cli]:
        groups = hooks.setdefault(event, [])
        if not isinstance(groups, list):
            raise ValueError(f'the existing {event} hooks value must be an array')
        ours = {'hooks': [{'type': 'command', 'command': command, 'timeout': timeout}]}
        at = next((i for i, g in enumerate(groups) if is_ours(g)), None)
        if at is None:
            groups.append(ours)
        else:
            groups[at] = ours
    return settings


def strip(settings: dict) -> dict:
    hooks = settings.get('hooks')
    if isinstance(hooks, dict):
        for event in list(hooks):
            if isinstance(hooks[event], list):
                hooks[event] = [g for g in hooks[event] if not is_ours(g)]
                if not hooks[event]:
                    del hooks[event]
        if not hooks:
            del settings['hooks']
    return settings


def kimi_block(command: str) -> str:
    rows = [KIMI_BEGIN]
    for event, timeout in EVENTS['kimi']:
        rows += ['[[hooks]]', f'event = {json.dumps(event)}', f'command = {json.dumps(command)}',
                 f'timeout = {timeout}', '']
    return '\n'.join(rows + [KIMI_END]) + '\n'


KIMI_BLOCK = re.compile(re.escape(KIMI_BEGIN) + r'.*?' + re.escape(KIMI_END) + r'\n?', re.S)


def kimi_merge(text: str, command: str) -> str:
    """config.toml with our block once, everything else byte for byte as it was."""
    import tomllib
    base = KIMI_BLOCK.sub('', text).rstrip('\n')
    out = (base + '\n\n' if base else '') + kimi_block(command)
    parsed = tomllib.loads(out)  # refuse to write a config Kimi could not read
    ours = [h for h in parsed.get('hooks', []) if MARKER in str(h.get('command', ''))]
    if len(ours) != len(EVENTS['kimi']):
        raise ValueError('config.toml already defines `hooks` in a way a [[hooks]] block cannot join')
    return out


def kimi_strip(text: str) -> str:
    return KIMI_BLOCK.sub('', text).rstrip('\n') + '\n' if KIMI_BLOCK.search(text) else text


def write_text(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        shutil.copy2(path, path.with_name(f'{path.name}.omarchy-deck.{int(time.time())}.bak'))
    tmp = path.with_name(path.name + '.tmp')
    tmp.write_text(text)
    os.replace(tmp, path)


def load(path: Path) -> dict:
    if not path.exists():
        return {}
    value = json.loads(path.read_text() or '{}')
    if not isinstance(value, dict):
        raise ValueError(f'{path} is not a JSON object')
    return value


def write(path: Path, settings: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        shutil.copy2(path, path.with_name(f'{path.name}.omarchy-deck.{int(time.time())}.bak'))
    tmp = path.with_name(path.name + '.tmp')
    tmp.write_text(json.dumps(settings, indent=2, ensure_ascii=False) + '\n')
    os.replace(tmp, path)


def targets(which: str) -> list[str]:
    return list(CLI_NAMES) if which == 'all' else [which]


def install(which: str, home: Path | None = None) -> None:
    for cli in targets(which):
        path = settings_path(cli, home)
        if cli == 'kimi':
            write_text(path, kimi_merge(path.read_text() if path.exists() else '', hook_command(cli)))
            print(f'{cli}: hooks written to {path}')
            continue
        write(path, merge(load(path), cli, hook_command(cli)))
        print(f'{cli}: hooks written to {path}')


def uninstall(which: str, home: Path | None = None) -> None:
    for cli in targets(which):
        path = settings_path(cli, home)
        if cli == 'kimi':
            if path.exists():
                write_text(path, kimi_strip(path.read_text()))
                print(f'{cli}: our hooks removed from {path}')
            continue
        if path.exists():
            write(path, strip(load(path)))
            print(f'{cli}: our hooks removed from {path}')


def status(home: Path | None = None) -> None:
    for cli in CLI_NAMES:
        path = settings_path(cli, home)
        if cli == 'kimi':
            try:
                text = path.read_text() if path.exists() else ''
            except OSError as error:
                print(f'{cli}: {path}: {error}')
                continue
            print(f'{cli}: {"installed" if KIMI_BLOCK.search(text) else "absent"} ({path})')
            continue
        try:
            hooks = load(path).get('hooks', {})
        except (OSError, ValueError) as error:
            print(f'{cli}: {path}: {error}')
            continue
        found = sorted(e for e, groups in hooks.items() if isinstance(groups, list) and any(map(is_ours, groups)))
        want = [e for e, _ in EVENTS[cli]]
        state = 'installed' if found == sorted(want) else (f'partial {found}' if found else 'absent')
        print(f'{cli}: {state} ({path})')
    print(f'daemon socket: {control_socket_path()} ({"present" if control_socket_path().exists() else "absent"})')


def main(argv: list[str]) -> int:
    if len(argv) >= 3 and argv[1] == 'hook' and argv[2] in CLI_NAMES:
        return run_hook(argv[2])
    verb = argv[1] if len(argv) > 1 else ''
    which = argv[2] if len(argv) > 2 else 'all'
    if verb in ('install', 'uninstall') and which in (*CLI_NAMES, 'all'):
        (install if verb == 'install' else uninstall)(which)
        return 0
    if verb == 'status':
        status()
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == '__main__':
    sys.exit(main(sys.argv))
