#!/usr/bin/env python3
"""Render every ui-preview scene to PNG and lay the mockup scenes over the mockups.

    tools/preview-all.py [--out DIR] [--mock DIR] [--board lilygo|hw-a|hw-b] [SCENE ...]

Builds native/core's `ui-preview` example for the host (offline, locked), asks
it for its scene table (`--list`), renders each scene to DIR/<scene>.png
(default DIR: target/preview) and, for the eight scenes that reproduce a
mockup, writes DIR/sheet-<scene>.png:

    [ mockup scaled to 480 px | firmware render | 50 % blend of the two ]

Read the sheets as images: in the blend, anything that does not line up shows
as a ghost. Mockups are looked up in --mock, $OWT_MOCK_DIR, then docs/mockups; without
them only the plain PNGs are written. `1-face-strip.png` shows the strip at
rest, so its sheet pairs `slide-50` with the two half faces cut from it.

Naming scenes limits the run to those. Needs Pillow. Exit status is non-zero
if any scene fails to render.
"""
import argparse
import os
import pathlib
import subprocess
import sys

from PIL import Image

ROOT = pathlib.Path(__file__).resolve().parent.parent

# scene -> (mockup file, crop box in mockup pixels or None)
MOCKUPS = {
    "roster": ("6-roster.png", None),
    "ptt-roster": ("7-channel-ptt.png", None),
    "card-question": ("8-question-card.png", None),
    "omarchy": ("3-omarchy-remote.png", None),
    "macropad": ("4-macropad.png", None),
    "usage": ("5-usage-report.png", None),
    "manager": ("2-face-manager.png", None),
    "slide-50": ("1-face-strip.png", None),
}


def build(board):
    host = subprocess.run(
        ["rustc", "-vV"], check=True, capture_output=True, text=True
    ).stdout.split("host: ")[1].split()[0]
    command = [
        "cargo", "build", "--manifest-path", "native/core/Cargo.toml",
        "--example", "ui-preview", "--target", host,
        "--target-dir", "target/native-core-preview", "--locked", "--offline",
    ]
    if board != "lilygo":
        command += ["--features", board]
    subprocess.run(command, check=True, cwd=ROOT)
    return ROOT / "target/native-core-preview" / host / "debug/examples/ui-preview"


def mockup(directory, name, crop):
    image = Image.open(directory / name).convert("RGB")
    if name == "1-face-strip.png":
        # The strip at rest: Agents spans x -240..720, Omarchy 760..1720. The
        # slide scene shows Agents' right half leaving, Omarchy's left arriving.
        left = image.crop((240, 0, 720, 960))
        right = image.crop((760, 0, 1240, 960))
        image = Image.new("RGB", (960, 960))
        image.paste(left, (0, 0))
        image.paste(right, (480, 0))
    elif crop:
        image = image.crop(crop)
    return image.resize((480, 480), Image.LANCZOS)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--out", default=str(ROOT / "target/preview"))
    parser.add_argument("--mock", default=os.environ.get("OWT_MOCK_DIR", str(ROOT / "docs" / "mockups")))
    parser.add_argument("--board", choices=["lilygo", "hw-a", "hw-b"], default="lilygo")
    parser.add_argument("--language", choices=["en", "zh-Hans", "zh-Hant", "ja"], default="en")
    parser.add_argument("scenes", nargs="*")
    args = parser.parse_args()

    preview = build(args.board)
    table = subprocess.run(
        [preview, "--list"], check=True, capture_output=True, text=True
    ).stdout.split()
    scenes = args.scenes or table
    unknown = [scene for scene in scenes if scene not in table]
    if unknown:
        sys.exit(f"preview-all: unknown scene(s): {' '.join(unknown)}")

    out = pathlib.Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    mock_dir = pathlib.Path(args.mock)
    failed = []
    sheets = 0
    for scene in scenes:
        ppm = out / f"{scene}.ppm"
        result = subprocess.run([preview, ppm, scene, args.language], capture_output=True, text=True)
        if result.returncode != 0:
            failed.append(scene)
            print(f"FAIL  {scene}: {result.stderr.strip().splitlines()[-1:]}")
            continue
        render = Image.open(ppm).convert("RGB")
        render.save(out / f"{scene}.png")
        ppm.unlink()
        if scene in MOCKUPS and (mock_dir / MOCKUPS[scene][0]).exists():
            reference = mockup(mock_dir, *MOCKUPS[scene])
            sheet = Image.new("RGB", (480 * 3 + 16, 480), (40, 40, 40))
            sheet.paste(reference, (0, 0))
            sheet.paste(render, (488, 0))
            sheet.paste(Image.blend(reference, render, 0.5), (976, 0))
            sheet.save(out / f"sheet-{scene}.png")
            sheets += 1
    print(f"preview-all: {len(scenes) - len(failed)} scenes, {sheets} sheets in {out}")
    if not mock_dir.is_dir():
        print(f"preview-all: no mockups at {mock_dir} (use --mock or OWT_MOCK_DIR)")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
