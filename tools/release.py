#!/usr/bin/env python3
"""Validate versions and build release assets consumed by the app updater."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
APP_BASE = 0x12020000
APP_SIZE = 16 * 1024 * 1024 - 128 * 1024  # span includes protected FF padding
BONDS_START, BONDS_END = 0x12700000, 0x12720000
REPOSITORY = "conol-ai/openwalkietalkie"


def versions():
    def package_version(manifest, package):
        metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--manifest-path", str(ROOT / manifest), "--locked", "--no-deps", "--format-version", "1"], cwd=ROOT))
        return next(p["version"] for p in metadata["packages"] if p["name"] == package)
    host = package_version("host/Cargo.toml", "openwalkietalkie-app")
    firmware = package_version("Cargo.toml", "openwalkietalkie-fw")
    for version in (host, firmware):
        if not re.fullmatch(r"\d+\.\d+\.\d+", version):
            raise ValueError("release versions must be MAJOR.MINOR.PATCH")
    return host, firmware


def validate_site(tag, public=None):
    """Require the release's static website content before publishing assets."""
    public = public if public is not None else ROOT / "site/public"
    atom = {"a": "http://www.w3.org/2005/Atom"}
    feed = ET.parse(public / "blog/feed.xml")
    entries = [entry for entry in feed.findall("a:entry", atom)
               if (entry.findtext("a:title", "", atom)).startswith(f"OpenWalkieTalkie {tag}:")]
    if len(entries) != 1:
        raise ValueError(f"website needs one blog/feed.xml entry for {tag}")
    link = entries[0].find("a:link", atom)
    url = link.get("href", "") if link is not None else ""
    match = re.fullmatch(r"https://openwalkietalkie\.org(/blog/[a-z0-9-]+/)", url)
    if not match:
        raise ValueError(f"website blog URL is invalid for {tag}")
    path = match[1]
    post = public / path.lstrip("/") / "index.html"
    if not post.is_file() or f"https://github.com/{REPOSITORY}/releases/tag/{tag}" not in post.read_text():
        raise ValueError(f"website needs a blog post linking to {tag}: {path}")
    if f'href="{path}"' not in (public / "blog/index.html").read_text():
        raise ValueError(f"website blog index must link to {path}")
    sitemap = ET.parse(public / "sitemap.xml")
    if url not in [loc.text for loc in sitemap.findall(".//{http://www.sitemaps.org/schemas/sitemap/0.9}loc")]:
        raise ValueError(f"website sitemap must include {path}")
    home = (public / "index.html").read_text()
    for arch in ("aarch64", "x86_64"):
        download = f"https://github.com/{REPOSITORY}/releases/download/{tag}/OpenWalkieTalkie-{tag[1:]}-macos-{arch}.dmg"
        if f'href="{download}"' not in home:
            raise ValueError(f"website {arch} download must point to {tag}")
    if tuple(map(int, tag.removeprefix("v").split("."))) >= (0, 11, 0):
        validate_companion_docs(public)


def validate_companion_docs(public):
    """The 0.11.0 contract must ship with discoverable, matching documentation."""
    guide_path = "/docs/native-companions/"
    required = ["docs/index.html", "docs/native-companions/index.html",
                "docs/deckd-api.md", "examples/native_companion.py"]
    for name in required:
        if not (public / name).is_file():
            raise ValueError(f"website native companion documentation missing: {name}")
    guide = (public / "docs/native-companions/index.html").read_text()
    for marker in ['name="owt-api-version" content="0.11.0"',
                   "agent.companion.set", "agent.session.set", "session.settings_requested",
                   'href="/examples/native_companion.py"', 'href="/docs/deckd-api.md"']:
        if marker not in guide:
            raise ValueError(f"website native companion guide must contain {marker}")
    if 'href="/docs/"' not in (public / "index.html").read_text():
        raise ValueError("website home must link to /docs/")
    if f'href="{guide_path}"' not in (public / "docs/index.html").read_text():
        raise ValueError("website docs index must link to native companion guide")
    sitemap = ET.parse(public / "sitemap.xml")
    urls = [loc.text for loc in sitemap.findall(".//{http://www.sitemaps.org/schemas/sitemap/0.9}loc")]
    for path in ["/docs/", guide_path]:
        if "https://openwalkietalkie.org" + path not in urls:
            raise ValueError(f"website sitemap must include {path}")
    for source, target in [("host/examples/native_companion.py", "examples/native_companion.py"),
                           ("docs/deckd-api.md", "docs/deckd-api.md")]:
        if (ROOT / source).read_bytes() != (public / target).read_bytes():
            raise ValueError(f"website download must match {source}: {target}")


def image_from_elf(data, board, bootstrap=False):
    if len(data) < 52 or data[:7] != b"\x7fELF\x01\x01\x01":
        raise ValueError("expected a 32-bit little-endian ELF")
    phoff = struct.unpack_from("<I", data, 28)[0]
    size, count = struct.unpack_from("<HH", data, 42)
    if size < 32 or phoff + size * count > len(data):
        raise ValueError("invalid ELF program headers")
    segments, executable = [], False
    for i in range(count):
        kind, offset, vaddr, paddr, length, _, flags, _ = struct.unpack_from("<8I", data, phoff + i * size)
        if kind != 1 or not length:
            continue
        if offset + length > len(data):
            raise ValueError("truncated ELF segment")
        if APP_BASE <= paddr < APP_BASE + APP_SIZE:
            if paddr + length > APP_BASE + APP_SIZE:
                raise ValueError("segment exceeds application flash")
            if paddr < BONDS_END and paddr + length > BONDS_START:
                raise ValueError("segment overlaps protected Bluetooth storage")
            segments.append((paddr, data[offset:offset + length]))
            # Rev A copies into PSRAM; other boards execute in place.
            # Board identity, not memory placement, selects the hardware.
            # Accept both layouts for recovery/downgrades too.
            executable |= bool(flags & 1) and (
                vaddr == paddr or 0x60000000 <= vaddr < 0x60800000
            )
        elif not 0x20000000 <= paddr < 0x70000000:
            raise ValueError("segment targets bootloader or another flash area")
    segments.sort()
    if not segments or segments[0][0] != APP_BASE or not executable:
        raise ValueError("image must start at application base and contain flash or PSRAM code")
    end = max(addr + len(body) for addr, body in segments)
    image = bytearray(b"\xff" * (end - APP_BASE))
    previous = APP_BASE
    for addr, body in segments:
        if addr < previous:
            raise ValueError("overlapping flash segments")
        image[addr - APP_BASE:addr - APP_BASE + len(body)] = body
        previous = addr + len(body)
    validate_image(image, board, bootstrap)
    return image


def marker(kind, board):
    return f"OpenWalkieTalkie {kind} Rev {'B' if board == 'hw-b' else 'A'}".encode()


def validate_image(image, board, bootstrap=False):
    if board not in ("hw-a", "hw-b") or not 8 <= len(image) <= APP_SIZE:
        raise ValueError("invalid board or image size")
    sp, pc = struct.unpack_from("<II", image)
    if not 0x20000000 <= sp <= 0x20080000 or not pc & 1 or not APP_BASE <= (pc & ~1) < APP_BASE + len(image):
        raise ValueError("invalid application vector table")
    if BONDS_START <= (pc & ~1) < BONDS_END:
        raise ValueError("reset vector points into protected Bluetooth storage")
    if any(byte != 255 for byte in image[BONDS_START - APP_BASE:BONDS_END - APP_BASE]):
        raise ValueError("protected Bluetooth storage must be FF padding")
    markers = [name for name in ("hw-a", "hw-b") if
               b"OpenWalkieTalkie Rev " + (b"A" if name == "hw-a" else b"B") + b" (" in image]
    if markers != [board]:
        raise ValueError("firmware does not identify the expected board")
    temporary = [b for b in ("hw-a", "hw-b") if marker("Bootstrap 1", b) in image]
    if bootstrap:
        if temporary != [board] or len(image) > BONDS_START - APP_BASE:
            raise ValueError("bootstrap must identify its board and fit protocol-1 recovery")
    elif temporary:
        raise ValueError("temporary bootstrap cannot be released as normal firmware")


def validate_recovery(image, board):
    if board not in ("hw-a", "hw-b") or not 8 <= len(image) <= 65536:
        raise ValueError("invalid recovery board or size")
    sp, pc = struct.unpack_from("<II", image)
    if not 0x20000000 <= sp <= 0x20080000 or not pc & 1 or not 0x20020000 <= (pc & ~1) < 0x20020000 + len(image):
        raise ValueError("recovery vectors must point into its SRAM load window")
    if [b for b in ("hw-a", "hw-b") if marker("Recovery 2", b) in image] != [board]:
        raise ValueError("recovery does not identify the expected board and protocol")


def asset(directory, name, tag):
    path = directory / name
    return {"name": name, "url": f"https://github.com/{REPOSITORY}/releases/download/{tag}/{name}",
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(), "size": path.stat().st_size}


def manifest(directory, tag):
    app, firmware = versions()
    if tag != f"v{app}":
        raise ValueError("tag does not match host workspace version")
    value = firmware_manifest(directory, tag, firmware)
    value["app"] = {"version": app, "macos": {arch: asset(directory, f"OpenWalkieTalkie-{app}-macos-{arch}.dmg", tag)
                                               for arch in ("aarch64", "x86_64")}}
    return value


def firmware_manifest(directory, tag, firmware):
    variants, migrations = {}, {}
    for board in ("hw-a", "hw-b"):
        name = f"OpenWalkieTalkie-{firmware}-{board}.bin"
        validate_image((directory / name).read_bytes(), board)
        variants[board] = asset(directory, name, tag)
        bootstrap = f"OpenWalkieTalkie-{firmware}-bootstrap-{board}.bin"
        recovery = f"OpenWalkieTalkie-{firmware}-recovery-{board}.bin"
        validate_image((directory / bootstrap).read_bytes(), board, bootstrap=True)
        validate_recovery((directory / recovery).read_bytes(), board)
        migrations[board] = {"bootstrap": asset(directory, bootstrap, tag),
                             "bootloader": asset(directory, recovery, tag)}
    return {"schema": 1, "product": "openwalkietalkie",
            "release_url": f"https://github.com/{REPOSITORY}/releases/tag/{tag}",
            "firmware": {"version": firmware, "protocol": 2, "app_base": APP_BASE,
                         "app_size": APP_SIZE, "variants": variants, "migrations": migrations}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    validate = sub.add_parser("validate")
    validate.add_argument("tag")
    build = sub.add_parser("image")
    build.add_argument("board", choices=["hw-a", "hw-b"])
    build.add_argument("elf", type=Path)
    build.add_argument("output", type=Path)
    build.add_argument("--bootstrap", action="store_true")
    recovery = sub.add_parser("recovery")
    recovery.add_argument("board", choices=["hw-a", "hw-b"])
    recovery.add_argument("binary", type=Path)
    recovery.add_argument("output", type=Path)
    release = sub.add_parser("manifest")
    release.add_argument("tag")
    release.add_argument("directory", type=Path)
    firmware_only = sub.add_parser("firmware-manifest", help="validate a local firmware bundle without app packages")
    firmware_only.add_argument("tag")
    firmware_only.add_argument("directory", type=Path)
    args = parser.parse_args()
    app, firmware = versions()
    if args.command == "validate":
        if args.tag != f"v{app}":
            raise ValueError("release tag must match host/Cargo.toml workspace version")
        validate_site(args.tag)
        subprocess.run(["git", "merge-base", "--is-ancestor", "HEAD", "origin/main"], cwd=ROOT, check=True)
        lines = f"tag={args.tag}\napp_version={app}\nfirmware_version={firmware}\n"
        if os.environ.get("GITHUB_OUTPUT"):
            with open(os.environ["GITHUB_OUTPUT"], "a") as output:
                output.write(lines)
        print(lines, end="")
    elif args.command == "image":
        args.output.mkdir(parents=True, exist_ok=True)
        name = f"OpenWalkieTalkie-{firmware}-{'bootstrap-' if args.bootstrap else ''}{args.board}"
        data = args.elf.read_bytes()
        (args.output / f"{name}.bin").write_bytes(image_from_elf(data, args.board, args.bootstrap))
        (args.output / f"{name}.elf").write_bytes(data)
    elif args.command == "recovery":
        data = args.binary.read_bytes()
        validate_recovery(data, args.board)
        args.output.mkdir(parents=True, exist_ok=True)
        (args.output / f"OpenWalkieTalkie-{firmware}-recovery-{args.board}.bin").write_bytes(data)
    else:
        if args.tag != f"v{app}":
            raise ValueError("tag does not match host workspace version")
        firmware_only = args.command == "firmware-manifest"
        value = firmware_manifest(args.directory, args.tag, firmware) if firmware_only else manifest(args.directory, args.tag)
        name = "firmware-manifest.json" if firmware_only else "release-manifest.json"
        (args.directory / name).write_text(json.dumps(value, indent=2) + "\n")
        files = sorted(p for p in args.directory.iterdir() if p.is_file() and p.name != "SHA256SUMS")
        (args.directory / "SHA256SUMS").write_text("".join(f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n" for p in files))


if __name__ == "__main__":
    main()
