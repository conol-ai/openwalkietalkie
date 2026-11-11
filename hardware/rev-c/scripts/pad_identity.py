#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Prove the Rev C board carries Rev B's copper (KiCad Python, read-only).

usage: pad_identity.py [--report FILE]

Compares main/main-revc.kicad_pcb with ../rev-b/main/main-revb.kicad_pcb in
the board frame, so a footprint whose own frame was turned still has to put
the same pads in the same places:

- every footprint: reference, side, origin, value, DNP and exclusion flags
- every pad: number, net, centre, board-frame size, shape, type, drill, layers
- every footprint graphic and text item: layer and board-frame bounding box
- every track and via: ends, width, drill, layers, net
- every zone: layers, net, outline and stored fill
- every board-level graphic (outline, silkscreen text)

Footprint angles are reported, not compared: that is the one thing Rev C is
allowed to change.
"""
import argparse
import hashlib
import json
from collections import Counter
from pathlib import Path

import pcbnew

ROOT = Path(__file__).resolve().parents[1]
REV_B = ROOT.parent / "rev-b/main/main-revb.kicad_pcb"
REV_C = ROOT / "main/main-revc.kicad_pcb"


def mm(value):
    return round(pcbnew.ToMM(value), 4)


def box(item):
    b = item.GetBoundingBox()
    return (mm(b.GetLeft()), mm(b.GetTop()), mm(b.GetRight()), mm(b.GetBottom()))


def pad_key(board, pad):
    width, height = mm(pad.GetSize().x), mm(pad.GetSize().y)
    angle = round(pad.GetOrientationDegrees()) % 360
    if angle % 180 == 90:
        width, height = height, width
    layers = tuple(sorted(board.GetLayerName(layer) for layer in pad.GetLayerSet().Seq()))
    return (pad.GetNumber(), pad.GetNetname(), mm(pad.GetPosition().x), mm(pad.GetPosition().y), width, height,
            angle % 90, pad.ShowPadShape(pcbnew.F_Cu), pad.ShowPadAttr(), mm(pad.GetDrillSize().x),
            mm(pad.GetDrillSize().y), layers)


def describe(path):
    board = pcbnew.LoadBoard(str(path))
    footprints = {}
    for f in board.GetFootprints():
        items = Counter((board.GetLayerName(g.GetLayer()), g.GetClass(), box(g)) for g in f.GraphicalItems())
        texts = Counter((board.GetLayerName(t.GetLayer()), t.GetText(), mm(t.GetTextPos().x), mm(t.GetTextPos().y))
                        for t in f.GetFields())
        footprints[f.GetReference()] = {
            "frame": (board.GetLayerName(f.GetLayer()), mm(f.GetPosition().x), mm(f.GetPosition().y), f.GetValue(),
                      f.IsDNP(), f.IsExcludedFromBOM(), f.IsExcludedFromPosFiles(), str(f.GetFPID().GetLibItemName())),
            "angle": round(f.GetOrientationDegrees(), 3) % 360,
            "pads": Counter(pad_key(board, p) for p in f.Pads()),
            "graphics": items, "texts": texts,
        }
    tracks, vias = Counter(), Counter()
    for t in board.GetTracks():
        if t.GetClass() == "PCB_VIA":
            vias[(mm(t.GetPosition().x), mm(t.GetPosition().y), mm(t.GetWidth(pcbnew.F_Cu)), mm(t.GetDrill()),
                  board.GetLayerName(t.TopLayer()), board.GetLayerName(t.BottomLayer()), t.GetNetname())] += 1
        else:
            ends = sorted([(mm(t.GetStart().x), mm(t.GetStart().y)), (mm(t.GetEnd().x), mm(t.GetEnd().y))])
            tracks[(t.GetClass(), tuple(ends), mm(t.GetWidth()), board.GetLayerName(t.GetLayer()), t.GetNetname())] += 1
    zones = Counter()
    for z in board.Zones():
        layers = tuple(sorted(board.GetLayerName(layer) for layer in z.GetLayerSet().Seq()))
        fill = []
        for layer in z.GetLayerSet().Seq():
            poly = z.GetFilledPolysList(layer)
            fill.append((board.GetLayerName(layer), poly.OutlineCount(), poly.FullPointCount(), round(poly.Area() / 1e12, 4)))
        outline = z.Outline()
        points = tuple((mm(outline.CVertex(i).x), mm(outline.CVertex(i).y)) for i in range(outline.FullPointCount()))
        zones[(layers, z.GetNetname(), z.GetZoneName(), z.GetIsRuleArea(), points, tuple(fill))] += 1
    drawings = Counter((board.GetLayerName(d.GetLayer()), d.GetClass(), box(d),
                        d.GetText() if hasattr(d, "GetText") else "") for d in board.GetDrawings())
    return {"footprints": footprints, "tracks": tracks, "vias": vias, "zones": zones, "drawings": drawings}


def compare():
    b, c = describe(REV_B), describe(REV_C)
    errors, turned = [], []
    if b["footprints"].keys() != c["footprints"].keys():
        errors.append(f"reference sets differ: {sorted(b['footprints'].keys() ^ c['footprints'].keys())}")
    pads = 0
    for ref in sorted(b["footprints"].keys() & c["footprints"].keys()):
        left, right = b["footprints"][ref], c["footprints"][ref]
        pads += sum(left["pads"].values())
        for field in ("frame", "pads", "graphics", "texts"):
            if left[field] != right[field]:
                errors.append(f"{ref}: {field} differ")
        if left["angle"] != right["angle"]:
            turned.append({"ref": ref, "rev_b_angle": left["angle"], "rev_c_angle": right["angle"]})
    for kind in ("tracks", "vias", "zones", "drawings"):
        if b[kind] != c[kind]:
            errors.append(f"{kind} differ: {sum(((b[kind] - c[kind]) + (c[kind] - b[kind])).values())} items")
    return {
        "passed": not errors,
        "rev_b_pcb_sha256": hashlib.sha256(REV_B.read_bytes()).hexdigest(),
        "rev_c_pcb_sha256": hashlib.sha256(REV_C.read_bytes()).hexdigest(),
        "footprints": len(c["footprints"]), "pads_compared": pads,
        "pad_fields": ["number", "net", "centre", "board-frame size", "shape", "type", "drill", "layers"],
        "tracks": sum(c["tracks"].values()), "vias": sum(c["vias"].values()), "zones": sum(c["zones"].values()),
        "board_graphics": sum(c["drawings"].values()),
        "footprint_frames_turned": turned,
        "errors": errors,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    report = compare()
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    if report["errors"]:
        raise SystemExit("\n".join(report["errors"]))
    turned = ", ".join(f"{t['ref']} {t['rev_b_angle']:g} -> {t['rev_c_angle']:g}" for t in report["footprint_frames_turned"])
    print(f"Copper identity passed: {report['footprints']} footprints, {report['pads_compared']} pads, "
          f"{report['tracks']} tracks, {report['vias']} vias, {report['zones']} zones identical to Rev B. "
          f"Footprint frames turned: {turned or 'none'}.")


if __name__ == "__main__":
    main()
