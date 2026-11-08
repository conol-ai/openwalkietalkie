"""Hyprland calls shared by the Omarchy deck adapter (stdlib only).

Lua first, legacy dispatcher second: the same pairing Omarchy's own scripts use.
"""
from __future__ import annotations

import json
import subprocess


def hyprctl_json(*args: str):
    out = subprocess.run(['hyprctl', *args, '-j'], capture_output=True, text=True, timeout=3)
    if out.returncode != 0:
        raise RuntimeError(out.stderr.strip() or f'hyprctl {args} failed')
    return json.loads(out.stdout or 'null')


def dispatch(lua: str, legacy: list[str]) -> None:
    """Lua first, as Omarchy's scripts do; the legacy dispatcher on a Hyprland without Lua."""
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
