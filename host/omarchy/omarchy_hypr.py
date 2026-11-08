"""Hyprland calls shared by the Omarchy deck adapter (stdlib only).

Lua first, legacy dispatcher second: the same pairing Omarchy's own scripts use.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import socket
import subprocess


def runtime_dir() -> Path:
    return Path(os.environ.get('XDG_RUNTIME_DIR') or f'/run/user/{os.getuid()}')


def instance_alive(signature: str) -> bool:
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.settimeout(0.5)
    try:
        sock.connect(str(runtime_dir() / 'hypr' / signature / '.socket.sock'))
        return True
    except OSError:
        return False
    finally:
        sock.close()


def ensure_instance() -> None:
    """Point HYPRLAND_INSTANCE_SIGNATURE at a running Hyprland.

    A service started by systemd (or an SSH session) has no such variable, and
    the one it inherited goes stale when the compositor restarts: the newest
    instance whose command socket answers wins.
    """
    os.environ.setdefault('XDG_RUNTIME_DIR', str(runtime_dir()))
    if not os.environ.get('WAYLAND_DISPLAY'):  # wtype needs it; a service does not inherit it
        for sock in sorted(runtime_dir().glob('wayland-*')):
            if not sock.name.endswith('.lock'):
                os.environ['WAYLAND_DISPLAY'] = sock.name
                break
    current = os.environ.get('HYPRLAND_INSTANCE_SIGNATURE')
    if current and instance_alive(current):
        return
    try:
        found = sorted((p for p in (runtime_dir() / 'hypr').iterdir() if p.is_dir()),
                       key=lambda p: p.stat().st_mtime, reverse=True)
    except OSError:
        return
    for path in found:
        if instance_alive(path.name):
            os.environ['HYPRLAND_INSTANCE_SIGNATURE'] = path.name
            return


def hyprctl_json(*args: str):
    ensure_instance()
    out = subprocess.run(['hyprctl', *args, '-j'], capture_output=True, text=True, timeout=3)
    if out.returncode != 0:
        raise RuntimeError(out.stderr.strip() or f'hyprctl {args} failed')
    return json.loads(out.stdout or 'null')


def dispatch(lua: str, legacy: list[str]) -> None:
    """Lua first, as Omarchy's scripts do; the legacy dispatcher on a Hyprland without Lua."""
    ensure_instance()
    ok = subprocess.run(['hyprctl', 'dispatch', lua], capture_output=True, timeout=3)
    if ok.returncode == 0 and ok.stdout.strip() == b'ok':
        return
    subprocess.run(['hyprctl', 'dispatch', *legacy], capture_output=True, timeout=3)


def focus_window(address: str) -> None:
    dispatch(f'hl.dsp.focus({{ window = "address:{address}" }})', ['focuswindow', f'address:{address}'])


def focus_slot(slot: int) -> None:
    # `o.workspaces.focus` is Omarchy's own per-monitor helper, the one Super+<n> runs.
    dispatch(f'o.workspaces.focus({slot})', ['workspace', str(slot)])


class Hypr:
    """What the session bridge needs from the compositor; tests replace it."""

    def clients(self) -> list:
        return hyprctl_json('clients') or []

    def active_address(self) -> str | None:
        active = hyprctl_json('activewindow')
        return (active or {}).get('address')

    def focus(self, address: str) -> None:
        focus_window(address)
