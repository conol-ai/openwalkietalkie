#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Rev C placement-preview package for JLCPCB's assembly preview (Python 3).

usage: build_preview.py --measure-preview MEASURE_PREVIEW.py [--out DIR] [--check] [--allow-dirty]
       build_preview.py --self-test

The package is what CoHDL's `tools/frame_boards/jlc_preview.py` reads: the
Gerber archive, an LCSC BOM, an LCSC CPL, `board.json`, a checklist,
`verification.json` and `SHA256SUMS`, flat in one ZIP. It is for the preview
only (the flow stops at Component Placements). Nothing here uploads anything.

MEASURE_PREVIEW.py is CoHDL's `tools/frame_boards/measure_preview.py`; its
`board` command (run with KiCad's Python, KICAD_PYTHON) writes the footprint
and pad positions of main/main-revc.kicad_pcb, and its `is_chip` decides which
parts the tool checks by their drawn body.

Inputs, all committed:

- fab-jlcpcb/main-bom-jlcpcb.csv, main-cpl-jlcpcb.csv,
  main-placement-sources.csv, main-revc-jlcpcb-gerber.zip   (build_fab.py)
- preview/corrections.csv   part number on a footprint -> rotate / offset, the
                            evidence for it and whether it is confirmed or
                            under test in this package
- preview/left-out.csv      fitted parts that cannot pass the BOM step
- preview/stock-risks.csv   parts kept in spite of low or unread stock

The preview CPL is the fab CPL (KiCad's absolute origin, Y negated, bottom
rotation mirrored: 180 - KiCad angle) with:

- the rows of left-out.csv removed; a part left out cannot be previewed;
- a "library binding" row unchanged: the fab row already carries the binding's
  map, and corrections.csv must state that same map;
- a "Rev C preview 2026-11-12" (or "board-4 raw reading") row with its map
  applied by `apply_map`, which is CoHDL's supplier CPL: `compose_place` of the
  placement with the map, then the `lcsc=mirrored` bottom convention.
  The offset (Dx, Dy) is in the footprint's own frame, +y down as KiCad draws
  the footprint. On the bottom its x is mirrored first; then it is turned by
  the placement's rotation and added to the position. The map's rotate does
  not turn the offset. In the mirrored convention the rotate adds to the CPL
  rotation on both sides. Only quarter turns are applied;
- every other row at its convention value, to be measured.

Every run first checks `apply_map` against the J5 binding (a known top-side
case) and bottom-side cases worked by hand from the compiler's rules;
--self-test does only that.

The preview BOM has one line per LCSC number, because the tool expects each
part number in one row of the BOM step.

Without --out it writes preview/manifest.json and preview/CHECKLIST.md; with
--check it writes nothing and those two must be current. With --out DIR it
also writes DIR/owt-revc-preview.zip and the same files unpacked in
DIR/owt-revc-preview/, and refuses a working tree with uncommitted changes
unless --allow-dirty (the package then says so).
"""
import argparse
import csv
import hashlib
import importlib.util
import io
import json
import os
import re
import subprocess
import sys
import tempfile
import zipfile
from collections import OrderedDict
from decimal import Decimal
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FAB = ROOT / "fab-jlcpcb"
PREVIEW = ROOT / "preview"
BOARD = ROOT / "main/main-revc.kicad_pcb"
NAME = "owt-revc"
LIBRARY, BOARD4, REVC, NONE = "library binding", "board-4 raw reading", "Rev C preview 2026-11-12", "none"
CONFIRMED, UNDER_TEST = "confirmed", "under test"
KICAD_PYTHON = "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/Current/bin/python3"
BINDING_MAP = re.compile(r"binding map at \(([-\d.]+)mm, ([-\d.]+)mm\) rotate (\d+) on ")


def rows(path):
    with path.open(newline="") as stream:
        return list(csv.DictReader(stream))


def natural(ref):
    return (re.match(r"[A-Za-z]+", ref).group(), int(re.search(r"\d+", ref).group()))


def render(header, body):
    stream = io.StringIO()
    writer = csv.writer(stream)  # CRLF, as in the fab files
    writer.writerow(header)
    writer.writerows(body)
    return stream.getvalue()


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def quarter_turn(x, y, deg):
    """CoHDL's `trig::rotate` for a quarter turn."""
    if deg % 90:
        raise ValueError("only quarter turns are applied")
    return {0: (x, y), 90: (-y, x), 180: (-x, -y), 270: (y, -x)}[deg % 360]


def apply_map(x, y, layer, rotation, rotate, dx, dy):
    """A supplier map (rotate, offset in the footprint's own frame) on a CPL row that has none.

    The steps are CoHDL's: the row back to the compiler's placement, `compose_place`
    (src/placement.rs), then `emit_cpl` with `lcsc=mirrored` (src/emit/cpl.rs).
    """
    bottom = layer == "Bottom"
    # The CPL row as the compiler's placement: +y down; on the bottom the
    # mirrored CPL rotation is 360 - the placement's rotation.
    px, py, pr = x, -y, (360 - rotation) % 360 if bottom else rotation % 360
    # compose_place: mirror x on the bottom BEFORE rotating; the board-frame
    # rotation is the inverse angle; a bottom placement reverses the map's rotate.
    if bottom:
        dx = -dx
    rx, ry = quarter_turn(dx, dy, (360 - pr) % 360)
    mx, my = px + rx, py + ry
    mr = (pr - rotate) % 360 if bottom else (pr + rotate) % 360
    # emit_cpl: Y negated; bottom KiCad angle = rotate + 180, mirrored = 540 - that.
    return mx, -my, (540 - (mr + 180) % 360) % 360 if bottom else mr


def coordinate(value):
    value = value.quantize(Decimal("0.0001"))
    return str(abs(value) if value == 0 else value)


def self_test():
    """`apply_map` against the one case read on a board and cases worked from the compiler's rules."""
    D = Decimal
    cases = [
        # J5, usb 0.1.6 binding `at (0mm, -1.55mm)`, top, KiCad angle 180: the
        # fab row (28.0000, -5.5500) against the footprint origin (28.0000, -4.0000).
        ("J5 binding, top", ("28.0000", "-4.0000", "Top", 180, 0, "0", "-1.55"), ("28.0000", "-5.5500", 180)),
        # The four top rows the 2026-11-12 record states.
        ("top 90, rotate 180", ("33.0000", "-84.9000", "Top", 90, 180, "0", "-0.95"), ("32.0500", "-84.9000", 270)),
        ("top 0", ("17.0000", "-16.5000", "Top", 0, 0, "0", "-1.15"), ("17.0000", "-15.3500", 0)),
        ("top 270, rotate 180", ("3.7600", "-18.5000", "Top", 270, 180, "0", "-0.55"), ("4.3100", "-18.5000", 90)),
        ("top 0, +x", ("10.0000", "-20.0000", "Top", 0, 0, "1", "0"), ("11.0000", "-20.0000", 0)),
        # Bottom, by hand from compose_place and emit_cpl. CPL (10, -20) rotation
        # 90 is the placement at (10, 20) rotate 270 on the bottom. Map at (1, 2)
        # rotate 90: mirror x -> (-1, 2); the inverse of 270 is 90, and
        # rotate((-1, 2), 90) = (-2, -1); position (8, 19); rotate 270 - 90 = 180;
        # KiCad angle 0; mirrored 540 - 0 = 180. So CPL (8, -19) rotation 180.
        ("bottom 90, rotate 90", ("10.0000", "-20.0000", "Bottom", 90, 90, "1", "2"), ("8.0000", "-19.0000", 180)),
        # Bottom at rotation 0: +x of the footprint is -x of the board seen from the top.
        ("bottom 0, +x", ("10.0000", "-20.0000", "Bottom", 0, 0, "1", "0"), ("9.0000", "-20.0000", 0)),
        # Bottom at CPL rotation 270 (placement rotate 90), map at (0, -1) rotate 270:
        # no x to mirror; inverse 270; rotate((0, -1), 270) = (-1, 0); position
        # (9, 20); rotate 90 - 270 = 180; KiCad 0; mirrored 180.
        ("bottom 270, rotate 270", ("10.0000", "-20.0000", "Bottom", 270, 270, "0", "-1"), ("9.0000", "-20.0000", 180)),
        # A rotate alone adds to the mirrored CPL rotation on the bottom too.
        ("bottom 90, rotate 270 alone", ("15.0000", "-42.5000", "Bottom", 90, 270, "0", "0"), ("15.0000", "-42.5000", 0)),
    ]
    failed = []
    for name, (x, y, layer, rotation, rotate, dx, dy), want in cases:
        mx, my, mr = apply_map(D(x), D(y), layer, rotation, rotate, D(dx), D(dy))
        got = (coordinate(mx), coordinate(my), mr)
        if got != want:
            failed.append(f"apply_map self-test, {name}: got {got}, expected {want}")
    if failed:
        raise SystemExit("\n".join(failed))
    return len(cases)


def load_tool(path):
    spec = importlib.util.spec_from_file_location("measure_preview", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def board_doc(tool_path):
    """`measure_preview.py board` of the routed board, as the tool writes it."""
    python = os.environ.get("KICAD_PYTHON") or (KICAD_PYTHON if os.path.exists(KICAD_PYTHON) else "python3")
    with tempfile.TemporaryDirectory() as td:
        out = Path(td) / "board.json"
        run = subprocess.run([python, "-I", str(tool_path), "board", str(BOARD), str(out)], text=True, capture_output=True)
        if run.returncode or not out.is_file():
            raise SystemExit("measure_preview.py board failed\n" + run.stdout + run.stderr)
        return json.loads(out.read_text())


def check_type(tool, fp):
    """How jlc_preview.py will judge this instance, and what that needs from a person."""
    if tool.is_chip(fp):
        return "body"
    numbered = {p["num"] for p in fp["pads"] if p["num"]}
    if re.match(r"(U|MK)\d", fp["ref"]):
        return "pin 1 mark"
    return "by eye (two-terminal, no mark)" if numbered == {"1", "2"} else "by eye (mark not certain)"


def build(tool_path):
    tool = load_tool(tool_path)
    fab_cpl = {r["Designator"]: r for r in rows(FAB / "main-cpl-jlcpcb.csv")}
    sources = {r["Designator"]: r for r in rows(FAB / "main-placement-sources.csv")}
    fab_bom = rows(FAB / "main-bom-jlcpcb.csv")
    left_out = OrderedDict((r["Designator"], r) for r in rows(PREVIEW / "left-out.csv"))
    corrections = OrderedDict(((r["LCSC Part #"], r["Board footprint"]), r) for r in rows(PREVIEW / "corrections.csv"))
    stock = rows(PREVIEW / "stock-risks.csv")
    doc = board_doc(tool_path)
    fps = {fp["ref"]: fp for fp in doc["fps"]}

    problems = []
    if set(fab_cpl) != set(sources):
        problems.append("fab CPL and placement sources list different designators")
    problems += [f"{ref}: in left-out.csv but not a fab placement" for ref in left_out if ref not in fab_cpl]
    problems += [f"{ref}: left-out.csv names {row['LCSC Part #'] or 'no part'}, the fab package {sources[ref]['LCSC Part #'] or 'no part'}"
                 for ref, row in left_out.items() if ref in sources and row["LCSC Part #"] != sources[ref]["LCSC Part #"]]
    problems += [f"{ref}: no LCSC number and not in left-out.csv" for ref, s in sources.items()
                 if not s["LCSC Part #"] and ref not in left_out]
    kept = sorted(set(fab_cpl) - set(left_out), key=natural)
    used = set()
    cpl, checklist = [], []
    for ref in kept:
        fab, src, fp = fab_cpl[ref], sources[ref], fps.get(ref)
        if fp is None:
            problems.append(f"{ref}: not on the board")
            continue
        if fab["Layer"] != ("Bottom" if fp["bottom"] else "Top"):
            problems.append(f"{ref}: the fab CPL says {fab['Layer']}, the board the other side")
        if not any(p["num"] in ("1", "A1") for p in fp["pads"]):
            problems.append(f"{ref}: no pad 1 or A1 to compare a pin 1 mark with")
        key = (src["LCSC Part #"], fp["fpid"])
        row = corrections.get(key)
        x, y, rotation, evidence, state, applied = fab["Mid X"], fab["Mid Y"], int(fab["Rotation"]), NONE, "", ""
        bound = src["LCSC number source"] == LIBRARY
        if row is not None:
            used.add(key)
            evidence, state = row["Evidence"], row["State"]
            rotate, offset = int(row["Rotate"]), (Decimal(row["Dx mm"]), Decimal(row["Dy mm"]))
            if state not in (CONFIRMED, UNDER_TEST):
                problems.append(f"{ref}: unknown state {state!r}")
            if (state == UNDER_TEST) != bool(row["Look at"]):
                problems.append(f"{ref}: a row under test, and only such a row, says what to look at")
            if evidence == LIBRARY:
                m = BINDING_MAP.match(src["Position and rotation basis"])
                if not bound or not m or (Decimal(m.group(1)), Decimal(m.group(2)), int(m.group(3))) != (*offset, rotate):
                    problems.append(f"{ref}: corrections.csv does not state the library binding's map")
                applied = f"binding map rotate {rotate}, offset ({row['Dx mm']}, {row['Dy mm']}) mm, already in the fab row"
            elif evidence in (BOARD4, REVC):
                if bound:
                    problems.append(f"{ref}: bound by the library; its correction must be the binding")
                if rotate % 90 or rotation % 90:
                    problems.append(f"{ref}: only quarter turns are applied")
                else:
                    mx, my, rotation = apply_map(Decimal(x), Decimal(y), fab["Layer"], rotation, rotate, *offset)
                    x, y = coordinate(mx), coordinate(my)
                if (x, y, rotation) == (fab["Mid X"], fab["Mid Y"], int(fab["Rotation"])):
                    applied = "rotate 0, no offset: nothing applied"
                else:
                    applied = (f"rotate {rotate}, offset ({row['Dx mm']}, {row['Dy mm']}) mm applied: fab row "
                               f"({fab['Mid X']}, {fab['Mid Y']}) {fab['Rotation']} becomes ({x}, {y}) {rotation}")
            else:
                problems.append(f"{ref}: unknown evidence {evidence!r}")
        elif bound:
            problems.append(f"{ref}: bound by the library but missing from corrections.csv")
        cpl.append([ref, x, y, fab["Layer"], str(rotation)])
        checklist.append({"ref": ref, "side": fab["Layer"], "pn": src["LCSC Part #"], "fpid": fp["fpid"],
                          "x": x, "y": y, "fab": [fab["Mid X"], fab["Mid Y"], int(fab["Rotation"])], "bound": bound,
                          "rotation": rotation, "evidence": evidence, "state": state, "applied": applied,
                          "check": check_type(tool, fp)})
    problems += [f"corrections.csv: {pn} on {fpid} is not a previewed placement" for pn, fpid in corrections if (pn, fpid) not in used]

    # One BOM line per LCSC number.
    lines = OrderedDict()
    for line in fab_bom:
        refs = [r for r in line["Designator"].split(",") if r not in left_out]
        if refs:
            entry = lines.setdefault(line["LCSC Part #"], {"comment": [], "footprint": [], "refs": []})
            entry["refs"] += refs
            for field, value in (("comment", line["Comment"]), ("footprint", line["Footprint"])):
                if value not in entry[field]:
                    entry[field].append(value)
    bom = [[" / ".join(e["comment"]), ",".join(sorted(e["refs"], key=natural)), " / ".join(e["footprint"]), pn]
           for pn, e in lines.items()]
    if "" in lines:
        problems.append("a previewed BOM line has no LCSC number")
    if sorted(r for e in lines.values() for r in e["refs"]) != sorted(kept):
        problems.append("the preview BOM and CPL list different designators")
    part_refs = {pn: e["refs"] for pn, e in lines.items()}
    problems += [f"stock-risks.csv: {r['LCSC Part #']} is not in the preview BOM" for r in stock if r["LCSC Part #"] not in part_refs]
    if problems:
        raise SystemExit("\n".join(problems))

    preview_fps = [fp for fp in doc["fps"] if fp["ref"] in set(kept)]
    board_json = json.dumps({"bbox": doc["bbox"], "fps": preview_fps}, indent=1)
    x0, y0, x1, y1 = doc["bbox"]
    gerbers = (FAB / "main-revc-jlcpcb-gerber.zip").read_bytes()
    with zipfile.ZipFile(io.BytesIO(gerbers)) as archive:
        layers = sum(name.endswith("_Cu.gbr") for name in archive.namelist())
    by_check = OrderedDict()
    for item in checklist:
        by_check.setdefault(item["check"], []).append(item["ref"])
    moved = [c for c in checklist if [c["x"], c["y"], c["rotation"]] != c["fab"]]
    row_change = lambda c: {"lcsc": c["pn"], "side": c["side"], "fab": c["fab"], "preview": [c["x"], c["y"], c["rotation"]]}
    summary = {
        "name": NAME,
        "purpose": "placement preview only; stop at Component Placements; not an order",
        "supplier_preview": "not uploaded",
        "expect": {"layers": layers, "size_mm": [round(x1 - x0, 2), round(y1 - y0, 2)], "bom_lines": len(bom),
                   "placements": len(cpl), "top": sum(r[3] == "Top" for r in cpl), "bottom": sum(r[3] == "Bottom" for r in cpl)},
        "part_numbers": sorted(lines),
        "bottom_rotation": "mirrored (180 - KiCad angle), as the fab package",
        "left_out": [{"designator": ref, "lcsc": r["LCSC Part #"], "reason": r["Reason"]} for ref, r in left_out.items()],
        "not_assembled": "DNP sites, test points and mounting holes are absent, as in the fab package",
        "pcba_type": "Standard (Economic offers one assembly side on this board); the tool selects it",
        "corrections": {
            LIBRARY: [c["ref"] for c in checklist if c["evidence"] == LIBRARY],
            UNDER_TEST + ", map applied": {c["ref"]: row_change(c) for c in checklist if c["state"] == UNDER_TEST},
            CONFIRMED + ", map applied": {c["ref"]: row_change(c) for c in moved if c["state"] == CONFIRMED},
            CONFIRMED + ", nothing to apply": [c["ref"] for c in checklist
                                               if c["state"] == CONFIRMED and c["evidence"] != LIBRARY and c not in moved],
            NONE: [c["ref"] for c in checklist if c["evidence"] == NONE],
        },
        "tool_check": by_check,
        "stock_risks": [{"lcsc": r["LCSC Part #"], "designators": part_refs[r["LCSC Part #"]], "stock": r["Stock"],
                         "read_on": r["Read on"], "note": r["Note"]} for r in stock],
    }
    files = OrderedDict([
        (NAME + "-gerbers.zip", gerbers),
        (NAME + "-lcsc-bom.csv", render(["Comment", "Designator", "Footprint", "LCSC Part #"], bom).encode()),
        (NAME + "-lcsc-cpl.csv", render(["Designator", "Mid X", "Mid Y", "Layer", "Rotation"], cpl).encode()),
        ("board.json", board_json.encode()),
        ("corrections.csv", (PREVIEW / "corrections.csv").read_bytes()),
        ("left-out.csv", (PREVIEW / "left-out.csv").read_bytes()),
        ("stock-risks.csv", (PREVIEW / "stock-risks.csv").read_bytes()),
    ])
    files["CHECKLIST.md"] = checklist_md(summary, checklist, corrections).encode()
    manifest = dict(summary)
    manifest["sha256"] = {name: sha256(data) for name, data in files.items()}
    manifest["inputs_sha256"] = {
        "main/main-revc.kicad_pcb": sha256(BOARD.read_bytes()),
        "fab-jlcpcb/main-bom-jlcpcb.csv": sha256((FAB / "main-bom-jlcpcb.csv").read_bytes()),
        "fab-jlcpcb/main-cpl-jlcpcb.csv": sha256((FAB / "main-cpl-jlcpcb.csv").read_bytes()),
        "measure_preview.py": sha256(Path(tool_path).read_bytes()),
    }
    return files, manifest


def checklist_md(summary, checklist, corrections):
    e = summary["expect"]
    out = [
        "# OpenWalkieTalkie Rev C: placement-preview checklist", "",
        "For JLCPCB's assembly preview only. Stop at Component Placements. Do not save to the cart, quote or order.",
        "**Not uploaded.** An upload needs the owner's approval for this package, by its SHA-256.", "",
        f"Upload `{NAME}-gerbers.zip` as the Gerber, `{NAME}-lcsc-bom.csv` as the BOM and `{NAME}-lcsc-cpl.csv` as the CPL.",
        f"Expect {e['layers']} layers, {e['size_mm'][0]} x {e['size_mm'][1]} mm, {e['bom_lines']} BOM lines and "
        f"{e['placements']} placements: {e['top']} top, {e['bottom']} bottom. Assemble both sides.",
        "The run needs the **Standard** PCBA type: Economic offers one assembly side on this four-layer board. "
        "`jlc_preview.py` (conol-ai/cohdl#116) selects it.", ""]
    tested = [c for c in checklist if c["state"] == UNDER_TEST]
    out += [
        "## What this iteration tests", "",
        f"The preview of 2026-11-12 read {len(tested)} placements wrong with no mapping applied. This package places them "
        "with the mapping that reading implies. Each must now read right; until it does, the mapping is not confirmed. "
        "The three offsets are estimates (J1 and J3 +-0.3 mm, the switches +-0.2 mm), so read how far the leads sit from the pad centres.", "",
        "| LCSC | Parts | Mapping under test | CPL row: fab, then this package | Look at |", "|---|---|---|---|---|"]
    for (pn, fpid), r in corrections.items():
        if r["State"] == UNDER_TEST:
            parts = [c for c in tested if (c["pn"], c["fpid"]) == (pn, fpid)]
            change = "; ".join(f"{c['ref']} ({c['side'].lower()}) ({c['fab'][0]}, {c['fab'][1]}) {c['fab'][2]} -> "
                               f"({c['x']}, {c['y']}) {c['rotation']}" for c in parts)
            out.append(f"| {pn} | {', '.join(c['ref'] for c in parts)} | rotate {r['Rotate']}, offset ({r['Dx mm']}, {r['Dy mm']}) mm | "
                       f"{change} | {r['Look at']} |")
    out += [
        "", "An offset is in the footprint's own frame (+y down, as KiCad draws the footprint): where the assembler's origin has to sit "
        "relative to the footprint origin. On the board it is turned by the placement's rotation; on the bottom its x is mirrored first.",
        "Every other placement was confirmed on 2026-11-12 and is placed as it was then; it should read as it did.", "",
        "## Left out, so not previewed", "",
        "A part that is not in the BOM and CPL is not drawn. Its orientation stays unmeasured after this preview.", "",
        "| Designator | LCSC | Why |", "|---|---|---|"]
    out += [f"| {r['designator']} | {r['lcsc'] or 'none'} | {r['reason']} |" for r in summary["left_out"]]
    out += ["", "DNP sites (C52, C53, C58, L6, R44), test points and mounting holes are not assembled and are absent, as in the fab package.",
            "", "## Stock risks, kept in", "",
            "The preview tool stops at the BOM step on any stock warning. If one of these is short, move it to `left-out.csv` and rebuild.", "",
            "| LCSC | Designators | Stock | Read on | Note |", "|---|---|---|---|---|"]
    out += [f"| {r['lcsc']} | {', '.join(r['designators'])} | {r['stock']} | {r['read_on'] or 'n/a'} | {r['note']} |"
            for r in summary["stock_risks"]]
    out += ["", "## Corrections applied", "",
            "Rows not listed here are at the convention value (top = KiCad angle, bottom = 180 - KiCad angle) with no evidence.", "",
            "| LCSC | Board footprint | Rotate | Offset mm | Evidence | State | Detail |", "|---|---|---|---|---|---|---|"]
    out += [f"| {pn} | `{fpid}` | {r['Rotate']} | ({r['Dx mm']}, {r['Dy mm']}) | {r['Evidence']} | {r['State']} | {r['Detail']} |"
            for (pn, fpid), r in corrections.items()]
    out += ["", "## What the tool checks, and what needs reading by eye", "",
            "- `body`: a chip part; the tool reads its drawn body along its pads (mod 180) and centred.",
            "- `pin 1 mark`: the tool needs one pin 1 mark on the part; `measure_preview.py measure` gives its angle against pad 1.",
            "- `by eye`: the tool has no evidence type for it. Expect \"manual review\"; read it from a crop of the export.", ""]
    out += [f"- {kind}: {len(refs)}: {', '.join(refs)}" for kind, refs in summary["tool_check"].items()]
    out += ["", "On 2026-11-12 the tool sent U1, U2, U7 and U8 to \"manual review\" although each has a pin 1 mark (a chip part was "
            "nearer the mark), and passed J1 and J3 on the mark alone. `measure_preview.py measure` reads the marks whatever the "
            "tool's status says; read the leads of J1, J3 and U3 and the switches from crops.",
            "U4, J5, U1, U2, U10 and U20 were confirmed on 2026-11-12 with the mapping they have here: U4's pin 1 toward the lower "
            "left seen from the top, J5's shell pegs in their holes.",
            "", "## Placements", "",
            "\"Fab rotation\" is the fab CPL's, the convention value unless the row is a library binding. Mid X, Mid Y and "
            "\"Preview rotation\" are this package's.", "",
            "| Ref | Side | LCSC | Board footprint | Mid X | Mid Y | Fab rotation | Preview rotation | Evidence | State | Applied | Check | Reading |",
            "|---|---|---|---|---|---|---|---|---|---|---|---|---|"]
    out += [f"| {c['ref']} | {c['side']} | {c['pn']} | `{c['fpid']}` | {c['x']} | {c['y']} | "
            f"{'binding' if c['bound'] else c['fab'][2]} | {c['rotation']} | {c['evidence']} | {c['state']} | {c['applied']} | {c['check']} | |"
            for c in checklist]
    return "\n".join(out) + "\n"


README = """OpenWalkieTalkie Rev C placement-preview package, commit {commit}

For JLCPCB's assembly preview only. Stop at Component Placements; do not save
to the cart, quote or order. Not uploaded: an upload needs the owner's
approval for this package.

Upload {name}-gerbers.zip as the Gerber, {name}-lcsc-bom.csv as the BOM and
{name}-lcsc-cpl.csv as the CPL. Expect {layers} layers, {w} x {h} mm,
{bom_lines} BOM lines and {placements} placements: {top} top, {bottom} bottom.

The run needs the Standard PCBA type (Economic offers one assembly side on
this board); jlc_preview.py selects it.

CHECKLIST.md lists what this iteration tests, what was left out and why, the
corrections applied, and every placement. board.json is `measure_preview.py board` of the routed board,
limited to the previewed placements. SHA256SUMS covers every other file here.
"""


def git(*argv):
    return subprocess.run(["git", "-C", str(ROOT), *argv], text=True, capture_output=True, check=True).stdout.strip()


def write_package(out, files, manifest, allow_dirty):
    commit, dirty = git("rev-parse", "HEAD"), bool(git("status", "--porcelain"))
    if dirty and not allow_dirty:
        raise SystemExit("the working tree has uncommitted changes; commit them or pass --allow-dirty")
    members = OrderedDict(files)
    verification = {k: v for k, v in manifest.items() if k != "sha256"}
    verification.update({"commit": commit, "dirty": dirty, "checks": {
        "bom_and_cpl_list_the_same_designators": True, "cpl_sides_equal_the_board": True,
        "every_instance_has_pad_1": True, "corrections_all_used_and_bindings_restated": True}})
    members["verification.json"] = (json.dumps(verification, indent=2) + "\n").encode()
    e = manifest["expect"]
    members["README.txt"] = README.format(commit=commit[:7] + ("-dirty" if dirty else ""), name=NAME, layers=e["layers"],
                                          w=e["size_mm"][0], h=e["size_mm"][1], **{k: e[k] for k in ("bom_lines", "placements", "top", "bottom")}).encode()
    members["SHA256SUMS"] = "".join(f"{sha256(data)}  {name}\n" for name, data in sorted(members.items())).encode()
    out.mkdir(parents=True, exist_ok=True)
    package = out / (NAME + "-preview.zip")
    with zipfile.ZipFile(package, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, data in sorted(members.items()):
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            info.external_attr = 0o644 << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, data)
    unpacked = out / (NAME + "-preview")
    unpacked.mkdir(exist_ok=True)
    for stale in unpacked.iterdir():
        stale.unlink()
    for name, data in members.items():
        (unpacked / name).write_bytes(data)
    return package


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--measure-preview", type=Path)
    ap.add_argument("--self-test", action="store_true", help="check apply_map against its known cases and stop")
    ap.add_argument("--out", type=Path)
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--allow-dirty", action="store_true")
    args = ap.parse_args()
    cases = self_test()
    if args.self_test:
        print(f"apply_map: {cases} cases pass.")
        return
    if not args.measure_preview:
        ap.error("--measure-preview is required")
    files, manifest = build(args.measure_preview)
    committed = {PREVIEW / "manifest.json": (json.dumps(manifest, indent=1) + "\n").encode(),
                 PREVIEW / "CHECKLIST.md": files["CHECKLIST.md"]}
    stale = [p.name for p, data in committed.items() if not p.exists() or p.read_bytes() != data]
    if args.check or args.out:
        if stale:
            raise SystemExit("preview files are not current: " + ", ".join(stale) + "; run build_preview.py and commit")
    else:
        for path, data in committed.items():
            path.write_bytes(data)
    e = manifest["expect"]
    print(f"Preview {'current' if args.check or args.out else 'written'}: {e['placements']} placements ({e['top']} top, "
          f"{e['bottom']} bottom) on {e['bom_lines']} BOM lines; {len(manifest['left_out'])} left out; not uploaded.")
    if args.out:
        package = write_package(args.out.resolve(), files, manifest, args.allow_dirty)
        print(f"{package.name}: sha256 {sha256(package.read_bytes())}")


if __name__ == "__main__":
    main()
