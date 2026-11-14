#!/usr/bin/env python3
"""Package the Windows host, deckd.exe and the app, as the release's zip.

The zip holds one folder, OpenWalkieTalkie-<version>-windows-x86_64, with
the two programs in bin\\ (the app allows firmware installs only when it runs
from a folder named bin, and starts the deckd.exe beside it), the install
notes and the licence. A <zip>.sha256 is written beside it.

Neither program is signed. Both are checked before they are packed: x86-64,
the app a GUI program (a console program opens a console window, and
closing it ends the app) and deckd a console program (it is also the CLI).
The archive is deterministic: fixed member order and timestamps.
"""
import argparse
import hashlib
from pathlib import Path
import struct
import zipfile

ROOT = Path(__file__).resolve().parents[1]
MACHINE_AMD64 = 0x8664
SUBSYSTEM_GUI = 2
SUBSYSTEM_CONSOLE = 3
# Zip timestamps cannot predate 1980; one fixed stamp keeps rebuilds identical.
STAMP = (1980, 1, 1, 0, 0, 0)

README = """OpenWalkieTalkie {version} for Windows (preview)
================================================

This build is an unsigned preview for Windows 10 and 11 on x64.
Install guide: https://openwalkietalkie.org/docs/windows/

1. Unzip this folder somewhere you keep programs, for example
   %LOCALAPPDATA%\\Programs\\OpenWalkieTalkie. Keep bin\\ as it is: the app
   installs firmware only when it runs from a folder named bin.
2. Open bin\\OpenWalkieTalkie.exe. Windows SmartScreen says "Windows
   protected your PC" because the program is not signed: choose
   "More info", then "Run anyway". With Smart App Control turned on, Windows
   blocks unsigned programs and offers no way past it.
3. In the app, open Service and press Start. That starts bin\\deckd.exe, the
   service that talks to the deck, with no window.
4. Connect the deck with USB-C.

Not in this preview: voice, starting with Windows, Claude Code and Codex
integration, and automatic app updates. Firmware updates use the slower USB
transfer. New versions are published on the latest release page:
https://github.com/conol-ai/openwalkietalkie/releases/latest

Verify the download against the .sha256 file published beside the zip.
"""


def pe_header(data):
    """(machine, subsystem) of a PE image, or ValueError."""
    if len(data) < 0x40 or data[:2] != b"MZ":
        raise ValueError("not a Windows executable")
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if pe + 0x5E > len(data) or data[pe:pe + 4] != b"PE\0\0":
        raise ValueError("not a Windows executable")
    machine = struct.unpack_from("<H", data, pe + 4)[0]
    subsystem = struct.unpack_from("<H", data, pe + 0x5C)[0]
    return machine, subsystem


def check(data, name, subsystem):
    machine, actual = pe_header(data)
    if machine != MACHINE_AMD64:
        raise ValueError(f"{name} is not an x86-64 program")
    if actual != subsystem:
        want = "GUI" if subsystem == SUBSYSTEM_GUI else "console"
        raise ValueError(f"{name} must be a {want} program")


def package(version, deckd, app, output):
    folder = f"OpenWalkieTalkie-{version}-windows-x86_64"
    deckd_bytes, app_bytes = deckd.read_bytes(), app.read_bytes()
    check(deckd_bytes, "deckd.exe", SUBSYSTEM_CONSOLE)
    check(app_bytes, "the app", SUBSYSTEM_GUI)
    members = [
        ("LICENSE.txt", (ROOT / "LICENSE").read_bytes()),
        ("README.txt", README.format(version=version).replace("\n", "\r\n").encode()),
        ("bin/OpenWalkieTalkie.exe", app_bytes),
        ("bin/deckd.exe", deckd_bytes),
    ]
    output.mkdir(parents=True, exist_ok=True)
    archive = output / f"{folder}.zip"
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as zf:
        for name, data in members:
            info = zipfile.ZipInfo(f"{folder}/{name}", STAMP)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o644 << 16
            zf.writestr(info, data)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    (output / f"{archive.name}.sha256").write_text(f"{digest}  {archive.name}\n")
    return archive


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--version", required=True, help="the release version, without the v")
    parser.add_argument("--deckd", required=True, type=Path, help="the built deckd.exe")
    parser.add_argument("--app", required=True, type=Path, help="the built openwalkietalkie-app.exe")
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    print(package(args.version, args.deckd, args.app, args.output))


if __name__ == "__main__":
    main()
