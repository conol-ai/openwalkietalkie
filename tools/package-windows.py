#!/usr/bin/env python3
"""Package the Windows host, deckd.exe and the app, as the release's zip.

The zip holds one folder, OpenWalkieTalkie-<version>-windows-x86_64, with
the two programs in bin\\ (the app allows firmware installs only when it runs
from a folder named bin, and starts the deckd.exe beside it), the install
notes, the licence, and in licenses\\ the licences of the fonts and icons the
app has built in (the Mac app carries the same files). A <zip>.sha256 is
written beside it.

Neither program is signed. Both are checked before they are packed: x86-64,
the app a GUI program (a console program opens a console window, and
closing it ends the app) and deckd a console program (it is also the CLI).
The archive is deterministic on any platform: fixed member order, timestamps,
compression level and "made on" system.
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
# "Made on Unix", whichever system packs it, so the 0644 modes below mean what
# they say and a Windows runner's zip has the same bytes as a Linux rebuild.
CREATE_SYSTEM_UNIX = 3
COMPRESS_LEVEL = 9
# The fonts and icons built into the app, under the names the Mac app's
# Resources/licenses uses (tools/package-macos.sh).
THIRD_PARTY_LICENSES = [
    ("Geist-OFL.txt", "host/crates/openwalkietalkie-app/assets/fonts/OFL.txt"),
    ("NotoSans-OFL.txt", "assets/fonts/OFL.txt"),
    ("NotoSansMono-OFL.txt", "assets/fonts/NotoSansMono-OFL.txt"),
    ("Lucide-LICENSE.txt", "assets/lucide/LICENSE"),
    ("PressStart2P-OFL.txt", "site/public/fonts/PressStart2P-OFL.txt"),
]

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
To update, quit the app, end deckd.exe, and replace this folder with the new
version's (each version's folder has its own name). Settings stay in
%LOCALAPPDATA%\\OpenWalkieTalkie.

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
    ] + [(f"licenses/{name}", (ROOT / source).read_bytes()) for name, source in THIRD_PARTY_LICENSES]
    output.mkdir(parents=True, exist_ok=True)
    archive = output / f"{folder}.zip"
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as zf:
        for name, data in members:
            info = zipfile.ZipInfo(f"{folder}/{name}", STAMP)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.create_system = CREATE_SYSTEM_UNIX
            info.external_attr = 0o644 << 16
            # A ZipInfo of our own takes no level from the ZipFile: pass it here.
            zf.writestr(info, data, compresslevel=COMPRESS_LEVEL)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    # Bytes, not text: on the Windows runner text mode would end the line in
    # CRLF, and `shasum -c` would then look for "<name>.zip\r".
    (output / f"{archive.name}.sha256").write_bytes(f"{digest}  {archive.name}\n".encode())
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
