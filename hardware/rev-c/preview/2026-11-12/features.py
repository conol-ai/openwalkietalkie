#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Extra readings of a placement-preview export, beside measure_preview.py's.

usage: python3 -I -B features.py MEASURE_PREVIEW.py board.json top.bmp bottom.bmp OUT.json

MEASURE_PREVIEW.py is CoHDL's tools/frame_boards/measure_preview.py.

- "marks": every magenta cluster, in board mm, its size in pixels, and the
  footprints within whose reach it lies with the distance to each origin.
  `owner` is the nearest footprint that is not a chip part (a chip part has
  no pin 1 mark); `nearest` is the nearest of all, which is what
  jlc_preview.py at CoHDL 1fe7273 counts the mark for.
- "bodies": measure_preview's method 2 body reading, for the two-terminal
  parts that are not on a passive-CHIP_* land (the inductors and ferrites).
- "blobs": for the connectors and switches, the connected areas of the
  model's white (every channel above 225) and of its dark grey (channels
  within 12 of each other, 45 to 95), as boxes in the footprint's own frame
  (millimetres, KiCad axes, origin at the footprint origin, before the
  footprint's rotation; a bottom part is still in KiCad's top-view frame).
  Areas under 0.03 mm2 are dropped. Silkscreen is white too.
"""
import importlib.util
import json
import math
import sys


def load(tool):
    spec = importlib.util.spec_from_file_location("measure_preview", tool)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


def local(fp, dx, dy):
    """Board offset from the origin -> the footprint's own frame."""
    a = math.radians(fp["angle"])
    return dx * math.cos(a) - dy * math.sin(a), dx * math.sin(a) + dy * math.cos(a)


def blobs(m, view, fp, pred, r):
    pts = {(x, y): local(fp, dx, dy) for x, y, dx, dy in view.around(fp["x"], fp["y"], r)
           if 0 <= y < len(view.rows) and len(view.px(x, y)) == 3 and x >= 0 and pred(*view.px(x, y))}
    seen, out = set(), []
    for start in sorted(pts):
        if start in seen:
            continue
        seen.add(start)
        stack, comp = [start], []
        while stack:
            p = stack.pop()
            comp.append(pts[p])
            for n in ((p[0] + i, p[1] + j) for i in (-1, 0, 1) for j in (-1, 0, 1)):
                if n in pts and n not in seen:
                    seen.add(n)
                    stack.append(n)
        area = len(comp) / (view.sx * view.sy)
        if area >= 0.03:
            xs, ys = [c[0] for c in comp], [c[1] for c in comp]
            out.append({"area_mm2": round(area, 2), "x": [round(min(xs), 2), round(max(xs), 2)],
                        "y": [round(min(ys), 2), round(max(ys), 2)]})
    return sorted(out, key=lambda b: -b["area_mm2"])


def white(b, g, r):
    return min(b, g, r) > 225


def dark(b, g, r):
    return max(b, g, r) - min(b, g, r) <= 12 and 45 <= max(b, g, r) <= 95


def main(tool, board_json, top, bottom, out):
    m = load(tool)
    with open(board_json) as f:
        doc = json.load(f)
    o = m.outline(doc)
    views = {False: m.View(top, o, False), True: m.View(bottom, o, True)}
    res = {"calibration": {("bottom" if b else "top"): {"px_per_mm_x": round(v.sx, 4), "px_per_mm_y": round(v.sy, 4),
                                                         "board_px": [v.left, v.top, v.right, v.low]}
                           for b, v in views.items()},
           "marks": [], "bodies": {}, "blobs": {}}
    for b, v in views.items():
        for x, y, n in sorted(m.dots(v, o, b)):
            near = sorted((round(math.hypot(x - fp["x"], y - fp["y"]), 2), fp["ref"]) for fp in doc["fps"]
                          if fp["bottom"] == b and math.hypot(x - fp["x"], y - fp["y"]) <= m.reach(fp))
            chips = {fp["ref"] for fp in doc["fps"] if m.is_chip(fp)}
            own = [r for _, r in near if r not in chips]
            res["marks"].append({"side": "bottom" if b else "top", "x": round(x, 2), "y": round(y, 2), "px": n,
                                 "diameter_mm": round(2 * math.sqrt(n / math.pi) / v.sx, 2),
                                 "within_reach": [[r, d] for d, r in near],
                                 "nearest": near[0][1] if near else None, "owner": own[0] if own else None})
    for fp in sorted(doc["fps"], key=lambda f: f["ref"]):
        pads = [p for p in fp["pads"] if p["num"]]
        v = views[fp["bottom"]]
        if len(pads) == 2 and not m.is_chip(fp):
            res["bodies"][fp["ref"]] = dict(m.body(v, fp), ok=m.body_ok(m.body(v, fp)))
        if fp["ref"][0] == "J" or fp["ref"][:2] == "SW" or fp["ref"] == "U3":
            r = m.reach(fp) + 0.5
            res["blobs"][fp["ref"]] = {"kicad_angle": fp["angle"], "side": "bottom" if fp["bottom"] else "top",
                                       "pads": {p["num"] + ("" if p["num"].isdigit() else "@%d" % i): [round(c, 3) for c in local(fp, p["x"] - fp["x"], p["y"] - fp["y"])]
                                                for i, p in enumerate(pads) if fp["ref"] != "U3" or p["num"] in ("1", "31", "2", "30", "MP")},
                                       "white": blobs(m, v, fp, white, r)[:8], "dark": blobs(m, v, fp, dark, r)[:4]}
    with open(out, "w") as f:
        json.dump(res, f, indent=1)
        f.write("\n")


if __name__ == "__main__":
    if len(sys.argv) != 6:
        sys.exit(__doc__)
    main(*sys.argv[1:])
