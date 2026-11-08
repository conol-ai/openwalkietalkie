#!/usr/bin/env python3
"""Generate the shared Chinese/Japanese fallback atlas (Pillow + fontTools required).

The 26 px, 2 bpp atlas lives in the application's upper NOR bank. The renderer
scales it to each face with bilinear filtering.
Run with --check to verify the checked-in metrics and bitmap.
"""
import argparse
import hashlib
import pathlib
import struct
import tempfile

from PIL import Image, ImageDraw, ImageFont
from fontTools.ttLib import TTFont

ROOT = pathlib.Path(__file__).resolve().parents[1]
SOURCE = ROOT / "assets/fonts/NotoSansCJKsc-Regular.otf"
SHA256 = "2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b"
PX = 26
# Radicals, CJK punctuation, Japanese kana, Chinese Bopomofo, unified
# ideographs + Extension A, compatibility ideographs and full/halfwidth forms.
# Coverage follows the source cmap; unassigned codepoints are never rasterized
# as .notdef boxes. Reuse the same source and lookup for Chinese and Japanese.
RANGES = [(0x2E80, 0x2FDF), (0x3000, 0x30FF), (0x3100, 0x312F),
          (0x31A0, 0x31BF), (0x31F0, 0x31FF), (0x3400, 0x4DBF),
          (0x4E00, 0x9FFF), (0xF900, 0xFAFF), (0xFF01, 0xFF9F),
          (0xFFE0, 0xFFE6)]


def generate(directory):
    assert hashlib.sha256(SOURCE.read_bytes()).hexdigest() == SHA256, "unexpected source font"
    cmap = TTFont(SOURCE).getBestCmap()
    font = ImageFont.truetype(str(SOURCE), PX, layout_engine=ImageFont.Layout.BASIC)
    atlas, metrics = bytearray(), bytearray()
    above = below = 0
    for cp in sorted(cmap):
        if not any(lo <= cp <= hi for lo, hi in RANGES):
            continue
        canvas = Image.new("L", (PX * 4, PX * 4))
        ImageDraw.Draw(canvas).text((PX, PX * 2), chr(cp), font=font, fill=255, anchor="ls")
        advance = round(font.getlength(chr(cp)))
        box = canvas.getbbox()
        if box:
            left, top, right, bottom = box
            mask = canvas.crop(box)
            left, top = left - PX, PX * 2 - top
            # Vertical repeat marks in the punctuation block span two ems.
            # Keep them inside the same line box as every other character.
            if mask.height > PX:
                ratio = PX / mask.height
                mask = mask.resize((max(1, round(mask.width * ratio)), PX), Image.Resampling.LANCZOS)
                left, top = round(left * ratio), round(top * ratio)
            w, h = mask.size
        else:
            mask, left, top, w, h = Image.new("L", (0, 0)), 0, 0, 0, 0
        assert 0 <= w <= PX + 1 and 0 <= h <= PX
        # Fixed 12-byte records: codepoint, advance, left, top, w, h,
        # reserved, atlas offset. Little-endian, no Rust alignment needed.
        metrics += struct.pack("<HBbbBBxI", cp, advance, left, top, w, h, len(atlas))
        pixels = mask.tobytes()
        for y in range(h):
            for x in range(0, w, 4):
                byte = 0
                for i in range(4):
                    alpha = (pixels[y * w + x + i] * 3 + 127) // 255 if x + i < w else 0
                    byte |= alpha << (6 - 2 * i)
                atlas.append(byte)
        above, below = max(above, top), max(below, h - top)
    assert len(atlas) + len(metrics) < 5_200_000, "CJK font exceeds its flash budget"
    (directory / "cjk.bin").write_bytes(atlas)
    (directory / "cjk-metrics.bin").write_bytes(metrics)
    print(f"CJK: {len(metrics) // 12} glyphs, {len(atlas)} bitmap + {len(metrics)} metric bytes; above={above}, below={below}")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    destination = ROOT / "assets/fonts/gen"
    if not args.check:
        generate(destination)
        return
    with tempfile.TemporaryDirectory() as tmp:
        directory = pathlib.Path(tmp)
        generate(directory)
        for name in ["cjk.bin", "cjk-metrics.bin"]:
            assert (directory / name).read_bytes() == (destination / name).read_bytes(), f"stale {name}"
    print("CJK atlas and metrics are up to date")


if __name__ == "__main__":
    main()
