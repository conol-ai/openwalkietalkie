#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Crops of a placement-preview export around footprints, with our pads drawn in.

usage: python3 -I -B crop.py MEASURE_PREVIEW.py board.json top.bmp bottom.bmp OUT_DIR REF...

MEASURE_PREVIEW.py is CoHDL's tools/frame_boards/measure_preview.py; its View
maps board millimetres to export pixels. Each crop is the export's pixels,
doubled, around one footprint. Drawn over it: a cyan ring on our pad 1, a
yellow square on each other numbered pad, a white cross on the footprint
origin. Nothing else is altered. A bottom crop is the export's view from
below (mirrored left to right), as exported.
"""
import importlib.util
import json
import struct
import sys
import zlib


def png(path, w, h, rgb):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    raw = b"".join(b"\x00" + bytes(rgb[y * w * 3:(y + 1) * w * 3]) for y in range(h))
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
                + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def main(tool, board_json, top, bottom, out, refs, scale=2):
    spec = importlib.util.spec_from_file_location("measure_preview", tool)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    with open(board_json) as f:
        doc = json.load(f)
    views = {False: m.View(top, m.outline(doc), False), True: m.View(bottom, m.outline(doc), True)}
    x0, y0 = m.outline(doc)[:2]
    for fp in doc["fps"]:
        if fp["ref"] not in refs:
            continue
        v = views[fp["bottom"]]

        def to_px(bx, by):
            ix = (v.right - (bx - x0) * v.sx) if v.bottom else (v.left + (bx - x0) * v.sx)
            return ix, v.top + (by - y0) * v.sy
        r = m.reach(fp) + 1.0
        cx, cy = to_px(fp["x"], fp["y"])
        half = int(r * v.sx)
        left, upper, n = int(cx) - half, int(cy) - half, 2 * half
        H, W = len(v.rows), len(v.rows[0]) // v.bpp
        buf = bytearray(n * scale * n * scale * 3)

        def put(x, y, c):
            if 0 <= x < n * scale and 0 <= y < n * scale:
                buf[(y * n * scale + x) * 3:(y * n * scale + x) * 3 + 3] = bytes(c)
        for y in range(n):
            for x in range(n):
                sx_, sy_ = left + x, upper + y
                b, g, r_ = v.px(sx_, sy_) if 0 <= sx_ < W and 0 <= sy_ < H else (40, 40, 40)
                for j in range(scale):
                    for i in range(scale):
                        put(x * scale + i, y * scale + j, (r_, g, b))
        ox, oy = [int((a - b_) * scale) for a, b_ in zip((cx, cy), (left, upper))]
        for d in range(-6, 7):
            put(ox + d, oy, (255, 255, 255))
            put(ox, oy + d, (255, 255, 255))
        for p in fp["pads"]:
            if not p["num"]:
                continue
            px_, py_ = to_px(p["x"], p["y"])
            px_, py_ = int((px_ - left) * scale), int((py_ - upper) * scale)
            if p["num"] in ("1", "A1"):
                for t in range(720):
                    import math
                    for rad in (9, 10):
                        put(px_ + int(round(rad * math.cos(t * math.pi / 360))),
                            py_ + int(round(rad * math.sin(t * math.pi / 360))), (0, 255, 255))
            else:
                for d in range(-3, 4):
                    for e in (-3, 3):
                        put(px_ + d, py_ + e, (255, 255, 0))
                        put(px_ + e, py_ + d, (255, 255, 0))
        png("%s/%s-%s.png" % (out, fp["ref"], "bottom" if fp["bottom"] else "top"), n * scale, n * scale, buf)


if __name__ == "__main__":
    if len(sys.argv) < 7:
        sys.exit(__doc__)
    main(*sys.argv[1:6], set(sys.argv[6:]))
