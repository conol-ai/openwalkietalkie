#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Compare a freshly built CoHDL board and fitted BOM with the Rev C board.

Read-only. Checks reference sets, every connected and NC numbered pad, values,
population, package identities, component placements, primary pad geometry,
mounting holes, outline, and fitted manufacturer/MPN. Routing, copper zones,
stackup, stencil geometry, and DRC are outside this source parity check.

Rev C adds a board-frame comparison of every primary pad (centre, size as it
lies on the board, layers). A library that turns a land inside its own frame
compiles without a diagnostic; this is the check that catches it, whatever
angle the footprint then reports.
"""
import argparse
import csv
import hashlib
import json
from pathlib import Path

import pcbnew

ROOT = Path(__file__).resolve().parents[1]
PROJECT = ROOT / "source/main"
NAME = "openmicrodial-rev-c"
# Pads whose copper on the routed board is known to differ from the source
# land. R39's two pads were turned a quarter turn on the Rev B board (0.64 x
# 0.54 mm on the board; the passive::CHIP_0402 land is 0.54 x 0.64 mm), with
# the same centres. Rev C keeps the copper, so the difference is allowed for
# exactly these pads and exactly this transposition.
TRANSPOSED_PADS = {("R39", "1"), ("R39", "2")}


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def point(p):
    return (pcbnew.ToMM(p.x), pcbnew.ToMM(p.y))


def close(a, b, tolerance=0.001):
    return len(a) == len(b) and all(abs(x - y) <= tolerance for x, y in zip(a, b))


def board_frame(board, pad):
    """Centre, size as the pad lies on the board, and layers."""
    size = point(pad.GetSize())
    angle = round(pad.GetOrientationDegrees()) % 180
    if angle == 90:
        size = size[::-1]
    elif angle:
        size += (angle,)
    layers = tuple(sorted(board.GetLayerName(layer) for layer in pad.GetLayerSet().Seq()))
    return point(pad.GetPosition()) + size, layers


def pads(footprint):
    result = {}
    for pad in footprint.Pads():
        if pad.GetNumber():
            result.setdefault(pad.GetNumber(), []).append(pad)
    return result


def netmap(footprint):
    result = {}
    for number, copies in pads(footprint).items():
        names = {p.GetNetname() for p in copies}
        if len(names) != 1:
            raise ValueError(f"{footprint.GetReference()}.{number}: repeated pads disagree on net")
        name = names.pop()
        # The charge-status net crosses the Power port in the restored source.
        if name == "CHG_STATUS":
            name = "power::CHG_STATUS"
        # CoHDL names the isolated switch frame lands ML/MR; the released
        # KiCad custom pads leave their numbers empty. They must remain NC.
        if footprint.GetReference() in {"SW1", "SW2", "SW5", "SW6"} and number in {"ML", "MR"}:
            if name:
                raise ValueError(f"{footprint.GetReference()}.{number}: frame land must remain NC")
            continue
        result[number] = name
    return result


def package(footprint):
    name = str(footprint.GetFPID().GetLibItemName())
    for prefix in ("openmicrodial_rev_a-", "openmicrodial_rev_b-", "openmicrodial_rev_c-"):
        name = name.removeprefix(prefix)
    return name.replace("MountingHole_2.0mm_NPTH", "MountingHole_2mm_NPTH")


def bom(path, mpn_column):
    result = {}
    with path.open(newline="") as stream:
        for row in csv.DictReader(stream):
            for ref in row["Designator"].split(","):
                ref = ref.strip()
                if ref in result:
                    raise ValueError(f"Duplicate BOM reference: {ref}")
                result[ref] = (row["Manufacturer"], row[mpn_column])
    return result


def edges(board):
    result = []
    for item in board.GetDrawings():
        if item.GetLayer() != pcbnew.Edge_Cuts:
            continue
        ends = sorted([point(item.GetStart()), point(item.GetEnd())])
        geometry = ends[0] + ends[1]
        if item.GetShape() == pcbnew.SHAPE_T_ARC:
            geometry += point(item.GetArcMid())
        result.append((item.GetShapeStr(), geometry))
    return result


def validate(generated, fitted_bom):
    released = ROOT / "main/main-revc.kicad_pcb"
    actual = pcbnew.LoadBoard(str(released))
    compiled = pcbnew.LoadBoard(str(generated))
    a = {f.GetReference(): f for f in actual.GetFootprints()}
    b = {f.GetReference(): f for f in compiled.GetFootprints()}
    errors = []
    if a.keys() != b.keys():
        errors.append(f"References missing={sorted(a.keys() - b.keys())}, extra={sorted(b.keys() - a.keys())}")
    primary_pads = 0
    numbered_pads = 0
    board_frame_pads = 0
    for ref in sorted(a.keys() & b.keys()):
        left, right = a[ref], b[ref]
        try:
            lnet, rnet = netmap(left), netmap(right)
            numbered_pads += len(lnet)
            for number in sorted(lnet.keys() | rnet.keys()):
                if lnet.get(number) != rnet.get(number):
                    errors.append(f"{ref}.{number}: net {rnet.get(number)!r}, expected {lnet.get(number)!r}")
        except ValueError as error:
            errors.append(str(error))
        for label, expected, found in [
            ("value", left.GetValue(), right.GetValue()),
            ("DNP", left.IsDNP(), right.IsDNP()),
            ("BOM exclusion", left.IsExcludedFromBOM(), right.IsExcludedFromBOM()),
            ("position exclusion", left.IsExcludedFromPosFiles(), right.IsExcludedFromPosFiles()),
            ("package", package(left), package(right)),
            ("side", left.GetLayer(), right.GetLayer()),
        ]:
            if expected != found:
                errors.append(f"{ref}: {label} {found!r}, expected {expected!r}")
        if not close(point(left.GetPosition()), point(right.GetPosition())):
            errors.append(f"{ref}: placement differs")
        if abs((left.GetOrientationDegrees() - right.GetOrientationDegrees() + 180) % 360 - 180) > 0.001:
            errors.append(f"{ref}: rotation differs")
        lpads, rpads = pads(left), pads(right)
        for number in lpads.keys() & rpads.keys():
            # One copper land per pin is the usual case. Repeated EP copper /
            # paste windows and multipart switch lands are not shape-audited.
            if len(lpads[number]) != 1 or len(rpads[number]) != 1:
                continue
            lp, rp = lpads[number][0], rpads[number][0]
            primary_pads += 1
            if not close(point(lp.GetPosition()) + point(lp.GetSize()), point(rp.GetPosition()) + point(rp.GetSize())):
                errors.append(f"{ref}.{number}: pad center/size differs")
            (lgeo, llayers), (rgeo, rlayers) = board_frame(actual, lp), board_frame(compiled, rp)
            board_frame_pads += 1
            if (ref, number) in TRANSPOSED_PADS:
                rgeo = rgeo[:2] + rgeo[3:1:-1]
                if close(lgeo, board_frame(compiled, rp)[0]):
                    errors.append(f"{ref}.{number}: listed as transposed but matches the source; remove it from TRANSPOSED_PADS")
            if not close(lgeo, rgeo):
                errors.append(f"{ref}.{number}: pad copper differs on the board: source {rgeo}, board {lgeo}")
            if llayers != rlayers:
                errors.append(f"{ref}.{number}: pad layers {rlayers}, expected {llayers}")
        if ref.startswith("MH"):
            def holes(f):
                return sorted(point(p.GetPosition()) + point(p.GetDrillSize()) for p in f.Pads())
            lh, rh = holes(left), holes(right)
            if len(lh) != len(rh) or any(not close(x, y) for x, y in zip(lh, rh)):
                errors.append(f"{ref}: mounting hole differs")
        if right.IsDNP() and any(p.IsOnLayer(pcbnew.F_Paste) or p.IsOnLayer(pcbnew.B_Paste) for p in right.Pads()):
            errors.append(f"{ref}: DNP site has paste")

    unmatched = edges(compiled)
    for kind, geometry in edges(actual):
        match = next((i for i, (k, g) in enumerate(unmatched) if k == kind and close(geometry, g, 0.005)), None)
        if match is None:
            errors.append(f"Outline: missing {kind} {geometry}")
        else:
            unmatched.pop(match)
    if unmatched:
        errors.append(f"Outline: {len(unmatched)} unexpected edges")

    expected_bom = bom(ROOT / "main/assembly/bom.csv", "MPN")
    source_bom = bom(fitted_bom, "Comment")
    for ref in sorted(expected_bom.keys() | source_bom.keys()):
        if expected_bom.get(ref) != source_bom.get(ref):
            errors.append(f"{ref}: BOM {source_bom.get(ref)!r}, expected {expected_bom.get(ref)!r}")
    source_files = [PROJECT / "cohdl.toml", PROJECT / "cohdl.lock", PROJECT / "design.lock", PROJECT / "assembly.json"]
    source_files += sorted((PROJECT / "src").glob("*.cohdl")) + [PROJECT / "mechanical/main-outline.dxf"]
    return {
        "passed": not errors,
        "released_pcb_sha256": sha(released),
        "released_bom_sha256": sha(ROOT / "main/assembly/bom.csv"),
        "source_sha256": {str(p.relative_to(ROOT)): sha(p) for p in source_files},
        "footprints": len(b), "numbered_pads_checked": numbered_pads,
        "primary_pad_centers_and_sizes_checked": primary_pads,
        "board_frame_pads_checked": board_frame_pads,
        "known_transposed_pads": sorted(f"{r}.{n}" for r, n in TRANSPOSED_PADS),
        "fitted_parts": len(source_bom), "dnp_parts": sorted(r for r, f in b.items() if f.IsDNP()),
        "outline_edges": len(edges(compiled)),
        "normalizations": ["CHG_STATUS = power::CHG_STATUS", "Unconnected SKTD ML/MR frame lands have no number in the released PCB", "Project-local footprint namespace differs (rev_a, rev_b on the board; rev_c in the source)", "MountingHole_2mm_NPTH = MountingHole_2.0mm_NPTH"],
        "not_checked": ["routing", "copper zones and stackup", "DRC", "silkscreen and 3D models", "full pad shapes and stencil geometry"],
        "errors": errors,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--generated", type=Path, default=PROJECT / f"out/{NAME}.kicad_pcb")
    parser.add_argument("--bom", type=Path, default=PROJECT / f"out/{NAME}-fitted-bom.csv")
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    report = validate(args.generated, args.bom)
    if args.report:
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    if report["errors"]:
        raise SystemExit("\n".join(report["errors"]))
    print(f"Source parity passed: {report['footprints']} footprints, {report['numbered_pads_checked']} numbered pads, "
          f"{report['board_frame_pads_checked']} pads compared in the board frame, "
          f"{report['fitted_parts']} fitted parts, {len(report['dnp_parts'])} DNP sites, {report['outline_edges']} outline edges.")


if __name__ == "__main__":
    main()
