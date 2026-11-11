#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Rev C placement-preview package for JLCPCB's assembly preview (Python 3).

usage: build_preview.py --measure-preview MEASURE_PREVIEW.py [--out DIR] [--check] [--allow-dirty]

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
- preview/corrections.csv   part number on a footprint -> rotate / offset and
                            the evidence for it
- preview/left-out.csv      fitted parts that cannot pass the BOM step
- preview/stock-risks.csv   parts kept in spite of low or unread stock

The preview CPL is the fab CPL (KiCad's absolute origin, Y negated, bottom
rotation mirrored: 180 - KiCad angle) with:

- the rows of left-out.csv removed; a part left out cannot be previewed;
- a "library binding" row unchanged: the fab row already carries the binding's
  map, and corrections.csv must state that same map;
- a "board-4 raw reading" row turned by its Rotate. In the mirrored
  convention a map's rotate adds to the CPL rotation on both sides, as in
  CoHDL's supplier CPL (`compose_place`, then 180 - angle on the bottom);
- every other row at its convention value, to be measured.

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
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FAB = ROOT / "fab-jlcpcb"
PREVIEW = ROOT / "preview"
BOARD = ROOT / "main/main-revc.kicad_pcb"
NAME = "owt-revc"
LIBRARY, BOARD4, NONE = "library binding", "board-4 raw reading", "none"
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
        rotation, evidence, applied = int(fab["Rotation"]), NONE, ""
        bound = src["LCSC number source"] == LIBRARY
        if row is not None:
            used.add(key)
            evidence = row["Evidence"]
            rotate, offset = int(row["Rotate"]), (float(row["Dx mm"]), float(row["Dy mm"]))
            if evidence == LIBRARY:
                m = BINDING_MAP.match(src["Position and rotation basis"])
                if not bound or not m or (float(m.group(1)), float(m.group(2)), int(m.group(3))) != (*offset, rotate):
                    problems.append(f"{ref}: corrections.csv does not state the library binding's map")
                applied = f"binding map rotate {rotate}, offset ({row['Dx mm']}, {row['Dy mm']}) mm, already in the fab row"
            elif evidence == BOARD4:
                if bound:
                    problems.append(f"{ref}: bound by the library; its correction must be the binding")
                if offset != (0.0, 0.0) or rotate % 90:
                    problems.append(f"{ref}: only quarter-turn rotations are applied to unbound rows")
                rotation = (rotation + rotate) % 360
                applied = f"rotate {rotate} applied" if rotate else "rotate 0, nothing applied"
            else:
                problems.append(f"{ref}: unknown evidence {evidence!r}")
        elif bound:
            problems.append(f"{ref}: bound by the library but missing from corrections.csv")
        cpl.append([ref, fab["Mid X"], fab["Mid Y"], fab["Layer"], str(rotation)])
        checklist.append({"ref": ref, "side": fab["Layer"], "pn": src["LCSC Part #"], "fpid": fp["fpid"],
                          "x": fab["Mid X"], "y": fab["Mid Y"], "convention": int(fab["Rotation"]) if not bound else None,
                          "rotation": rotation, "evidence": evidence, "applied": applied, "check": check_type(tool, fp)})
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
    turned = [c for c in checklist if c["evidence"] == BOARD4 and c["rotation"] != c["convention"]]
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
        "corrections": {
            LIBRARY: [c["ref"] for c in checklist if c["evidence"] == LIBRARY],
            BOARD4 + ", rotation applied": {c["ref"]: {"lcsc": c["pn"], "convention": c["convention"], "preview": c["rotation"]}
                                            for c in turned},
            BOARD4 + ", rotate 0": [c["ref"] for c in checklist if c["evidence"] == BOARD4 and c not in turned],
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
        f"{e['placements']} placements: {e['top']} top, {e['bottom']} bottom. Assemble both sides.", "",
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
            "| LCSC | Board footprint | Rotate | Offset mm | Evidence | Detail |", "|---|---|---|---|---|---|"]
    out += [f"| {pn} | `{fpid}` | {r['Rotate']} | ({r['Dx mm']}, {r['Dy mm']}) | {r['Evidence']} | {r['Detail']} |"
            for (pn, fpid), r in corrections.items()]
    out += ["", "## What the tool checks, and what needs reading by eye", "",
            "- `body`: a chip part; the tool reads its drawn body along its pads (mod 180) and centred.",
            "- `pin 1 mark`: the tool needs one pin 1 mark on the part; `measure_preview.py measure` gives its angle against pad 1.",
            "- `by eye`: the tool has no evidence type for it. Expect \"manual review\"; read it from a crop of the export.", ""]
    out += [f"- {kind}: {len(refs)}: {', '.join(refs)}" for kind, refs in summary["tool_check"].items()]
    out += ["", "U4: pin 1 must land toward the lower left seen from the top. J5: the shell pegs must sit in their holes.",
            "U1, U2, U10 and U20 are placed with the candidate rotation: the model's leads must lie on the pads and its pin 1 on pad 1.",
            "", "## Placements", "",
            "| Ref | Side | LCSC | Board footprint | Mid X | Mid Y | Convention | Preview rotation | Evidence | Applied | Check | Reading |",
            "|---|---|---|---|---|---|---|---|---|---|---|---|"]
    out += [f"| {c['ref']} | {c['side']} | {c['pn']} | `{c['fpid']}` | {c['x']} | {c['y']} | "
            f"{'binding' if c['convention'] is None else c['convention']} | {c['rotation']} | {c['evidence']} | {c['applied']} | {c['check']} | |"
            for c in checklist]
    return "\n".join(out) + "\n"


README = """OpenWalkieTalkie Rev C placement-preview package, commit {commit}

For JLCPCB's assembly preview only. Stop at Component Placements; do not save
to the cart, quote or order. Not uploaded: an upload needs the owner's
approval for this package.

Upload {name}-gerbers.zip as the Gerber, {name}-lcsc-bom.csv as the BOM and
{name}-lcsc-cpl.csv as the CPL. Expect {layers} layers, {w} x {h} mm,
{bom_lines} BOM lines and {placements} placements: {top} top, {bottom} bottom.

CHECKLIST.md lists what was left out and why, the corrections applied, and
every placement. board.json is `measure_preview.py board` of the routed board,
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
    ap.add_argument("--measure-preview", type=Path, required=True)
    ap.add_argument("--out", type=Path)
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--allow-dirty", action="store_true")
    args = ap.parse_args()
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
