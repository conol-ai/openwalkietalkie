"""Macropad keystrokes as a keyboard the kernel knows: uinput (stdlib only).

wtype types through Wayland's virtual-keyboard protocol, and Hyprland runs none of its own
binds for such a keyboard: Omarchy's Super+S never fires from it, while the focused app does
get the keys. A uinput device is a keyboard like any other to the compositor, so its
keystrokes reach Hyprland's binds and the focused app alike.

`/dev/uinput` is root's by default; `71-openwalkietalkie-uinput.rules` gives it to the user at
the seat (`uaccess`). Without it, `UinputKeyboard.open()` fails and the bridge types with wtype,
which still reaches apps.

Key codes are the kernel's (`linux/input-event-codes.h`): positions on the keyboard, which the
compositor reads through the user's layout, as it does a physical keyboard's.
"""
from __future__ import annotations

import fcntl
import os
import struct
import time

# ioctl numbers, linux/uinput.h (x86-64 and arm64 alike).
UI_DEV_CREATE = 0x5501
UI_DEV_DESTROY = 0x5502
UI_DEV_SETUP = 0x405C5503   # _IOW('U', 3, struct uinput_setup), 92 bytes
UI_SET_EVBIT = 0x40045564   # _IOW('U', 100, int)
UI_SET_KEYBIT = 0x40045565  # _IOW('U', 101, int)
EV_SYN, EV_KEY, SYN_REPORT = 0, 1, 0
BUS_VIRTUAL = 0x06
# struct input_event on 64-bit Linux: struct timeval (two longs), u16 type, u16 code, s32 value.
EVENT = struct.Struct('llHHi')
# struct uinput_setup: struct input_id (4 x u16), char name[80], u32 ff_effects_max.
SETUP = struct.Struct('HHHH80sI')
NAME = b'OpenWalkieTalkie Macropad'
# How long the compositor needs to pick up a new keyboard before its first key.
SETTLE_S = 0.25
STEP_S = 0.004

MODIFIERS = {'ctrl': 29, 'shift': 42, 'opt': 56, 'alt': 56, 'cmd': 125}  # cmd: KEY_LEFTMETA (Super)
KEYS = {
    **dict(zip('1234567890', range(2, 12))),
    **dict(zip('qwertyuiop', range(16, 26))),
    **dict(zip('asdfghjkl', range(30, 39))),
    **dict(zip('zxcvbnm', range(44, 51))),
    **{f'f{n}': 58 + n for n in range(1, 11)}, 'f11': 87, 'f12': 88,
    'esc': 1, 'minus': 12, 'equal': 13, 'backspace': 14, 'tab': 15, 'lbracket': 26,
    'rbracket': 27, 'enter': 28, 'semicolon': 39, 'quote': 40, 'grave': 41, 'backslash': 43,
    'comma': 51, 'period': 52, 'slash': 53, 'space': 57, 'home': 102, 'up': 103,
    'pageup': 104, 'left': 105, 'right': 106, 'end': 107, 'down': 108, 'pagedown': 109,
    'delete': 111,
}


def codes(combo: str) -> tuple[list[int], int] | None:
    """`cmd+shift+4` -> (modifier codes, key code), else None: the grammar of deckd's
    `keystroke_valid`, modifiers in any order and exactly one key."""
    held: list[int] = []
    key = None
    for part in str(combo).split('+'):
        if part in MODIFIERS:
            if MODIFIERS[part] not in held:
                held.append(MODIFIERS[part])
        elif key is None and part in KEYS:
            key = KEYS[part]
        else:
            return None
    return (held, key) if key is not None else None


def events(combo: str) -> list[tuple[int, int, int]] | None:
    """The (type, code, value) events of one keystroke: modifiers down, the key down and up,
    modifiers up, each step followed by a SYN_REPORT."""
    parsed = codes(combo)
    if parsed is None:
        return None
    held, key = parsed
    out = []
    for code, value in [*((m, 1) for m in held), (key, 1), (key, 0), *((m, 0) for m in reversed(held))]:
        out += [(EV_KEY, code, value), (EV_SYN, SYN_REPORT, 0)]
    return out


class UinputKeyboard:
    """One virtual keyboard, made at the first keystroke and kept: a device made per press
    would have the compositor enumerate a new keyboard every time."""

    def __init__(self, path: str = '/dev/uinput', sleep=time.sleep):
        self.path = path
        self.sleep = sleep
        self.fd: int | None = None

    def open(self) -> None:
        """Raises OSError when /dev/uinput cannot be opened (no udev rule) or set up."""
        if self.fd is not None:
            return
        fd = os.open(self.path, os.O_WRONLY | os.O_NONBLOCK)
        try:
            fcntl.ioctl(fd, UI_SET_EVBIT, EV_KEY)
            for code in sorted({*MODIFIERS.values(), *KEYS.values()}):
                fcntl.ioctl(fd, UI_SET_KEYBIT, code)
            fcntl.ioctl(fd, UI_DEV_SETUP, SETUP.pack(BUS_VIRTUAL, 0x1209, 0x5754, 1, NAME, 0))
            fcntl.ioctl(fd, UI_DEV_CREATE)
        except OSError:
            os.close(fd)
            raise
        self.fd = fd
        self.sleep(SETTLE_S)

    def press(self, combo: str) -> bool:
        """Type `combo`; False when it is not a keystroke this keyboard has. OSError when the
        device cannot be made or written (the caller falls back to wtype)."""
        steps = events(combo)
        if steps is None:
            return False
        self.open()
        try:
            for kind, code, value in steps:
                os.write(self.fd, EVENT.pack(0, 0, kind, code, value))
                if kind == EV_SYN:
                    self.sleep(STEP_S)
        except OSError:
            self.close()
            raise
        return True

    def close(self) -> None:
        if self.fd is None:
            return
        try:
            fcntl.ioctl(self.fd, UI_DEV_DESTROY)
        except OSError:
            pass
        os.close(self.fd)
        self.fd = None
