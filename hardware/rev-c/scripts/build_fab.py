#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Rev C JLCPCB BOM, CPL, orientation list and sourcing record (KiCad Python).

usage: build_fab.py [--check]

Run after build_source.py. Inputs:

- source/main/out/*-lcsc-bom.csv, *-lcsc-cpl.csv, *-cpl.csv, *-lcsc-map.json
  from `cohdl build --supplier lcsc --bottom-rotation lcsc=mirrored`
- source/main/assembly.json          DNP sites and parts left out of assembly
- source/main/supplier-fallback.csv  designator -> LCSC number for placements
                                     the library does not bind (from Rev B's
                                     order BOM)
- source/main/frame-evidence.json    frame-evidence ids of the bindings used
- source/main/placement-evidence.csv part number on a board footprint ->
                                     rotate / offset, and the placement-preview
                                     record that read it on this board
- main/assembly/bom.csv              design BOM (value, footprint, design MPN)
- main/main-revc.kicad_pcb           footprint origins and pin-1 pads

Every fitted placement takes its LCSC number from exactly one place: the
library binding the compiler selected, or the fallback table. A placement
found in both, or in neither, stops the build. DNP sites, test points and
mounting holes never reach the BOM or the CPL.

Every CPL row has one stated basis (PLACEMENT_BASES), from
placement-evidence.csv, keyed by LCSC number and board footprint:

- library binding: the compiler's row; the table must state the binding's map;
- board-local mapping: an unbound row, footprint origin and convention angle,
  with the table's rotate and offset applied by build_preview.apply_map (the
  compiler's own supplier-CPL arithmetic). Board-local means the map is kept
  here, not in the CoHDL library;
- convention value confirmed by preview: an unbound row left as it is, which
  the placement preview read right (the table's map is zero);
- not previewed: an unbound row left as it is, absent from the table.

Writes fab-jlcpcb/main-bom-jlcpcb.csv, main-cpl-jlcpcb.csv,
main-placement-sources.csv, main-orientation-check.csv and
verification/fab-coverage.json. With --check nothing is written; the committed
files must equal what the source and library produce now.
"""
import csv
import io
import json
import math
import re
import sys
from collections import OrderedDict
from decimal import Decimal
from pathlib import Path

import pcbnew

sys.path.insert(0, str(Path(__file__).resolve().parent))
from build_preview import apply_map, coordinate  # noqa: E402  (plain Python; no upload, no side effects on import)

ROOT = Path(__file__).resolve().parents[1]
PROJECT = ROOT / "source/main"
OUT = PROJECT / "out"
NAME = "openmicrodial-rev-c"
FAB = ROOT / "fab-jlcpcb"
REV_B_FAB = ROOT.parent / "rev-b/fab-jlcpcb"
BINDING, FALLBACK, UNSOURCED = "library binding", "fallback table", "no LCSC part"
MAPPED, CONVENTION_OK, NOT_PREVIEWED = "board-local mapping", "convention value confirmed by preview", "not previewed"
PLACEMENT_BASES = (BINDING, MAPPED, CONVENTION_OK, NOT_PREVIEWED)


def rows(path):
    with path.open(newline="") as stream:
        return list(csv.DictReader(stream))


def per_designator(path):
    result = {}
    for row in rows(path):
        for ref in row["Designator"].split(","):
            ref = ref.strip()
            if ref in result:
                raise SystemExit(f"{path.name}: {ref} listed twice")
            result[ref] = row
    return result


def natural(ref):
    return (re.match(r"[A-Za-z]+", ref).group(), int(re.search(r"\d+", ref).group()))


def mm(text):
    return float(text.replace("mm", ""))


def render(header, body):
    stream = io.StringIO()
    writer = csv.writer(stream)  # CRLF, as in Rev B's order files
    writer.writerow(header)
    writer.writerows(body)
    return stream.getvalue()


def build():
    assembly = json.loads((PROJECT / "assembly.json").read_text())
    not_assembled = set(assembly["dnp"]) | set(assembly["exclude_from_bom"])
    supplier_map = json.loads((OUT / f"{NAME}-lcsc-map.json").read_text())
    if supplier_map.get("bottom_rotation") != "mirrored":
        raise SystemExit("Supplier CPL was not built with --bottom-rotation lcsc=mirrored")
    designators = supplier_map["designators"]
    supplier_bom = per_designator(OUT / f"{NAME}-lcsc-bom.csv")
    supplier_cpl = {r["Designator"]: r for r in rows(OUT / f"{NAME}-lcsc-cpl.csv")}
    neutral_cpl = {r["Designator"]: r for r in rows(OUT / f"{NAME}-cpl.csv")}
    design_bom = per_designator(ROOT / "main/assembly/bom.csv")
    fallback = {r["Designator"]: r for r in rows(PROJECT / "supplier-fallback.csv")}
    evidence = json.loads((PROJECT / "frame-evidence.json").read_text())["rows"]
    placement_evidence = {}
    for r in rows(PROJECT / "placement-evidence.csv"):
        key = (r["LCSC Part #"], r["Board footprint"])
        zero = (int(r["Rotate"]), Decimal(r["Dx mm"]), Decimal(r["Dy mm"])) == (0, 0, 0)
        if key in placement_evidence:
            raise SystemExit(f"placement-evidence.csv: {key} listed twice")
        if r["Basis"] not in (BINDING, MAPPED, CONVENTION_OK) or (r["Basis"] == MAPPED) == zero and r["Basis"] != BINDING:
            raise SystemExit(f"placement-evidence.csv: {key}: basis {r['Basis']!r} does not fit its map")
        if int(r["Rotate"]) % 90 or not (ROOT / r["Preview record"] / "README.md").is_file():
            raise SystemExit(f"placement-evidence.csv: {key}: not a quarter turn, or no such preview record")
        placement_evidence[key] = r
    used_evidence = set()
    board = pcbnew.LoadBoard(str(ROOT / "main/main-revc.kicad_pcb"))
    footprints = {f.GetReference(): f for f in board.GetFootprints()}

    fitted = sorted(set(footprints) - not_assembled, key=natural)
    if set(fitted) != set(design_bom):
        raise SystemExit(f"Fitted parts and design BOM differ: {sorted(set(fitted) ^ set(design_bom))}")
    bound = {ref for ref, d in designators.items() if d.get("pn")}
    for ref, d in designators.items():
        if (d.get("pn") or "") != supplier_bom[ref]["LCSC Part #"]:
            raise SystemExit(f"{ref}: supplier map and supplier BOM disagree")
    problems = []
    problems += [f"{r}: bound by the library and also in the fallback table" for r in sorted(bound & set(fallback))]
    problems += [f"{r}: fitted, not bound, and missing from the fallback table" for r in fitted if r not in bound and r not in fallback]
    problems += [f"{r}: in the fallback table but not a fitted part" for r in sorted(set(fallback) - set(fitted))]
    problems += [f"{r}: bound by the library but not assembled" for r in sorted(bound & not_assembled)]
    if problems:
        raise SystemExit("\n".join(problems))

    # The compiler's CPL origin is the outline's lower-left corner; the Gerbers
    # use KiCad's absolute origin. One translation must fit every footprint.
    def offset(ref):
        p = footprints[ref].GetPosition()
        return (pcbnew.ToMM(p.x) - mm(neutral_cpl[ref]["Mid X"]), -pcbnew.ToMM(p.y) - mm(neutral_cpl[ref]["Mid Y"]))
    ox, oy = offset(fitted[0])
    for ref in neutral_cpl:
        dx, dy = offset(ref)
        if abs(dx - ox) > 1e-4 or abs(dy - oy) > 1e-4:
            raise SystemExit(f"{ref}: source placement is not at the board's footprint origin")

    def frame_row(ref):
        d = designators[ref]
        version = supplier_map["footprints"][d["footprint"]]
        found = [e for e in evidence if e["supplier"] == "lcsc" and e["pn"] == d["pn"]
                 and e["footprint"] == d["footprint"].split("::")[-1]
                 and e["package"] == version["package"] and e["package_version"] == version["version"]]
        if len(found) != 1:
            raise SystemExit(f"{ref}: {len(found)} frame-evidence rows for {d['pn']} on {d['footprint']} "
                             f"({version['package']} {version['version']}); update frame-evidence.json")
        mapping = (found[0]["mapping"]["dx_mm"], found[0]["mapping"]["dy_mm"], found[0]["mapping"]["rotate_deg"])
        if mapping != (mm(d["at"][0]), mm(d["at"][1]), d["rotate"]):
            raise SystemExit(f"{ref}: frame-evidence mapping {mapping} is not the binding's map")
        return found[0], version

    rev_b_cpl = {r["Designator"]: r for r in rows(REV_B_FAB / "main-cpl-jlcpcb.csv")}
    rev_b_bom = per_designator(REV_B_FAB / "main-bom-jlcpcb.csv")
    cpl, sources, bom_lines = [], [], OrderedDict()
    differs = []
    for ref in fitted:
        f, d, design, c = footprints[ref], designators[ref], design_bom[ref], supplier_cpl[ref]
        x, y, rotation = mm(c["Mid X"]) + ox, mm(c["Mid Y"]) + oy, int(c["Rotation"])
        bottom = f.GetLayer() == pcbnew.B_Cu
        if c["Layer"] != ("Bottom" if bottom else "Top"):
            raise SystemExit(f"{ref}: side differs between source and board")
        convention = round((180 - f.GetOrientationDegrees()) % 360 if bottom else f.GetOrientationDegrees() % 360)
        origin = (f"{pcbnew.ToMM(f.GetPosition().x):.4f}", f"{-pcbnew.ToMM(f.GetPosition().y):.4f}")
        fpid = f.GetFPIDAsString()
        if ref in bound:
            row, version = frame_row(ref)
            lcsc, source, frame = d["pn"], BINDING, row["id"]
            placed = "" if d["mpn"] == design["MPN"] else f"{d['mfr']} {d['mpn']}"
            package = f"{version['package']} {version['version']}"
            note = (f"LCSC number and placement frame from the library binding on {d['part']}, land "
                    f"{d['footprint']} ({package}); frame evidence {frame} ({row['evidence']['method']}, "
                    f"{row['evidence']['date']}).")
            basis = f"binding map at ({d['at'][0]}, {d['at'][1]}) rotate {d['rotate']} on {d['footprint']} ({package})"
            pe = placement_evidence.get((lcsc, fpid))
            if pe is not None:
                used_evidence.add((lcsc, fpid))
                if pe["Basis"] != BINDING or (Decimal(pe["Dx mm"]), Decimal(pe["Dy mm"]), int(pe["Rotate"])) != (
                        Decimal(str(mm(d["at"][0]))), Decimal(str(mm(d["at"][1]))), d["rotate"]):
                    raise SystemExit(f"{ref}: placement-evidence.csv does not state the library binding's map")
                basis += f"; confirmed on this board in the placement preview ({pe['Preview record']})"
            placement, record = BINDING, pe["Preview record"] if pe else ""
        else:
            if rotation != convention or abs(x - pcbnew.ToMM(f.GetPosition().x)) > 1e-4:
                raise SystemExit(f"{ref}: unbound row is not the footprint origin with the convention angle")
            lcsc, placed, note = (fallback[ref][k] for k in ("LCSC Part #", "Part placed (if substituted)", "Note"))
            source, frame, package = (FALLBACK if lcsc else UNSOURCED), "", ""
            pe = placement_evidence.get((lcsc, fpid))
            convention_text = "footprint origin and convention angle (top = KiCad angle, bottom = 180 - KiCad angle)"
            if pe is None:
                placement, record = NOT_PREVIEWED, ""
                basis = convention_text + "; not checked against JLC's part model"
            elif pe["Basis"] == BINDING:
                raise SystemExit(f"{ref}: placement-evidence.csv calls it a library binding, the compiler did not bind it")
            else:
                used_evidence.add((lcsc, fpid))
                placement, record = pe["Basis"], pe["Preview record"]
                if placement == MAPPED:
                    mx, my, rotation = apply_map(Decimal(origin[0]), Decimal(origin[1]), c["Layer"], convention,
                                                 int(pe["Rotate"]), Decimal(pe["Dx mm"]), Decimal(pe["Dy mm"]))
                    x, y = float(coordinate(mx)), float(coordinate(my))
                    basis = (f"board-local map at ({pe['Dx mm']}mm, {pe['Dy mm']}mm) rotate {pe['Rotate']} on {fpid}, applied to the "
                             f"{convention_text}: ({origin[0]}, {origin[1]}) {convention}; read with the map applied in the "
                             f"placement preview ({record})")
                else:
                    basis = convention_text + f"; read right in the placement preview ({record})"
        cpl.append([ref, f"{x:.4f}", f"{y:.4f}", c["Layer"], str(rotation)])
        old = rev_b_cpl[ref]
        same = (old["Mid X"], old["Mid Y"], old["Layer"], old["Rotation"]) == tuple(cpl[-1][1:])
        if not same:
            differs.append({"designator": ref, "rev_b": [old["Mid X"], old["Mid Y"], old["Rotation"]],
                            "rev_c": [cpl[-1][1], cpl[-1][2], cpl[-1][4]]})
        sources.append([ref, lcsc, source, frame, d["part"], package, basis, cpl[-1][1], cpl[-1][2], cpl[-1][4],
                        old["Mid X"], old["Mid Y"], old["Rotation"], "same" if same else "DIFFERS",
                        "same" if rev_b_bom[ref]["LCSC Part #"] == lcsc else "DIFFERS",
                        placement, record, origin[0], origin[1], str(convention)])
        key = (design["Comment"], design["Footprint"], lcsc, design["MPN"], design["Manufacturer"], placed, note,
               source + (f": {frame}" if frame else ""))
        bom_lines.setdefault(key, []).append(ref)

    unused = sorted(set(placement_evidence) - used_evidence)
    if unused:
        raise SystemExit(f"placement-evidence.csv: rows that fit no fitted placement: {unused}")

    bom = [[k[0], ",".join(refs), k[1], k[2], len(refs), k[3], k[4], k[5], k[6], k[7]] for k, refs in bom_lines.items()]

    # Orientation list in Rev B's format; the rotation is the CPL's.
    directions = ["right", "upper right", "up", "upper left", "left", "lower left", "down", "lower right"]
    orientation = []
    cpl_rotation = {r[0]: r[4] for r in cpl}
    source_of = {r[0]: {BINDING: f"{r[2]} {r[3]}" + (f", confirmed by {r[16]}" if r[16] else ""),
                        MAPPED: f"board-local mapping, read in {r[16]}",
                        CONVENTION_OK: f"convention, confirmed by {r[16]}",
                        NOT_PREVIEWED: "convention, unverified (not previewed)"}[r[15]] for r in sources}
    for ref in fitted:
        f = footprints[ref]
        if not ref.startswith(("U", "J", "SW", "MK", "Y", "AE")):
            continue
        pin1 = next((p for p in f.Pads() if p.GetNumber() in ("1", "A1")), None)
        if pin1 is None:
            continue
        cu, cv = pcbnew.ToMM(f.GetPosition().x), pcbnew.ToMM(f.GetPosition().y)
        pu, pv = pcbnew.ToMM(pin1.GetPosition().x), pcbnew.ToMM(pin1.GetPosition().y)
        toward = directions[int(((math.degrees(math.atan2(-(pv - cv), pu - cu)) + 22.5) % 360) // 45)]
        boxes = [p.GetBoundingBox() for p in f.Pads()]
        x0, x1 = min(pcbnew.ToMM(b.GetLeft()) for b in boxes), max(pcbnew.ToMM(b.GetRight()) for b in boxes)
        y0, y1 = min(pcbnew.ToMM(b.GetTop()) for b in boxes), max(pcbnew.ToMM(b.GetBottom()) for b in boxes)
        orientation.append([ref, design_bom[ref]["MPN"], "Bottom" if f.GetLayer() == pcbnew.B_Cu else "Top",
                            cpl_rotation[ref], f"{cu:.3f}", f"{-cv:.3f}", f"{pu:.3f}", f"{-pv:.3f}", toward,
                            f"{math.hypot((x0 + x1) / 2 - cu, (y0 + y1) / 2 - cv):.2f}", source_of[ref]])

    sourced = [r for r in sources if r[1]]
    coverage = {
        "fitted_placements": len(fitted),
        "placements_with_lcsc_number": len(sourced),
        "library_bound": len([r for r in sources if r[2] == BINDING]),
        "fallback_table": len([r for r in sources if r[2] == FALLBACK]),
        "no_lcsc_part": [r[0] for r in sources if r[2] == UNSOURCED],
        "library_bound_designators": {r[0]: {"lcsc": r[1], "frame_evidence": r[3], "package": r[5]}
                                      for r in sources if r[2] == BINDING},
        "bom_lines": len(bom),
        "not_assembled_and_absent_from_outputs": sorted(not_assembled, key=natural),
        "lcsc_numbers_differing_from_rev_b_order_bom": [r[0] for r in sources if r[14] != "same"],
        "cpl_rows_differing_from_rev_b_order_cpl": differs,
        "cpl_origin_translation_mm": [round(ox, 4), round(oy, 4)],
        "placement_basis": {b: [r[0] for r in sources if r[15] == b] for b in PLACEMENT_BASES},
        "board_local_mappings": {r[0]: {"lcsc": r[1], "origin_and_convention": [r[17], r[18], r[19]],
                                        "cpl_row": [r[7], r[8], r[9]], "preview_record": r[16]}
                                 for r in sources if r[15] == MAPPED},
    }
    files = {
        FAB / "main-bom-jlcpcb.csv": render(["Comment", "Designator", "Footprint", "LCSC Part #", "Quantity", "Design MPN",
                                             "Design Manufacturer", "Part placed (if substituted)", "Note", "LCSC number source"], bom),
        FAB / "main-cpl-jlcpcb.csv": render(["Designator", "Mid X", "Mid Y", "Layer", "Rotation"], sorted(cpl, key=lambda r: r[0])),
        FAB / "main-placement-sources.csv": render(
            ["Designator", "LCSC Part #", "LCSC number source", "Frame evidence id", "CoHDL part", "Land package version",
             "Position and rotation basis", "Mid X", "Mid Y", "Rotation", "Rev B CPL Mid X", "Rev B CPL Mid Y",
             "Rev B CPL Rotation", "CPL row vs Rev B", "LCSC number vs Rev B", "Placement basis", "Preview record",
             "Footprint origin X", "Footprint origin Y", "Convention rotation"], sources),
        FAB / "main-orientation-check.csv": render(
            ["Designator", "Design MPN", "Layer", "JLC Rotation", "Centre X", "Centre Y", "Pin 1 X", "Pin 1 Y",
             "Pin 1 is toward (seen from top)", "Origin to pad-centre offset mm", "Rotation source"], orientation),
        ROOT / "verification/fab-coverage.json": json.dumps(coverage, indent=2) + "\n",
    }
    # Population check on what is about to be written, not on the inputs.
    for path, text in files.items():
        if path.suffix != ".csv":
            continue
        listed = set()
        for row in csv.DictReader(io.StringIO(text)):
            listed |= {r.strip() for r in row["Designator"].split(",")}
        if listed & not_assembled:
            raise SystemExit(f"{path.name}: lists parts that are not assembled: {sorted(listed & not_assembled)}")
        if path.name != "main-orientation-check.csv" and listed != set(fitted):
            raise SystemExit(f"{path.name}: does not list exactly the fitted parts")
    return files, coverage


def main():
    files, coverage = build()
    if "--check" in sys.argv:
        stale = [p.name for p, text in files.items() if not p.exists() or p.read_bytes() != text.encode()]
        if stale:
            raise SystemExit("Committed order files differ from the source and library output: " + ", ".join(stale))
    else:
        for path, text in files.items():
            path.write_bytes(text.encode())
    print(f"Order data {'matches' if '--check' in sys.argv else 'written'}: {coverage['fitted_placements']} placements on "
          f"{coverage['bom_lines']} BOM lines; {coverage['library_bound']} of {coverage['placements_with_lcsc_number']} "
          f"LCSC numbers from library bindings, {coverage['fallback_table']} from the fallback table; "
          f"{len(coverage['not_assembled_and_absent_from_outputs'])} unassembled sites left out; "
          f"{len(coverage['cpl_rows_differing_from_rev_b_order_cpl'])} CPL rows differ from Rev B's. Placement basis: "
          + "; ".join(f"{len(v)} {k}" for k, v in coverage["placement_basis"].items()) + ".")


if __name__ == "__main__":
    main()
