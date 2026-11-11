#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Build the Rev C CoHDL project and apply its assembly metadata to outputs.

Run with KiCad's Python (pcbnew), or use ../check.sh. The routed PCB is never
written. The build selects the LCSC supplier bindings and JLC's bottom-side
rotation convention, so out/ also holds the supplier BOM, CPL and map that
build_fab.py turns into the order package. CoHDL has no DNP syntax, so
assembly.json supplies population flags and display labels; circuit
connectivity comes from CoHDL.
"""
import csv
import json
import os
from pathlib import Path
import subprocess

import pcbnew

ROOT = Path(__file__).resolve().parents[1]
PROJECT = ROOT / "source/main"
OUT = PROJECT / "out"
NAME = "openmicrodial-rev-c"
SUPPLIER = ["--supplier", "lcsc", "--bottom-rotation", "lcsc=mirrored"]


def build():
    compiler = os.environ.get("COHDL_BIN", "cohdl")
    expected = (ROOT / ".cohdl-version").read_text().strip()
    version = subprocess.check_output([compiler, "--version"], text=True).strip()
    if version.split()[1] != expected:
        raise SystemExit(f"Expected CoHDL {expected}, got {version}; set COHDL_BIN.")
    # Fetch the locked dependency versions (hash-checked) before building.
    subprocess.run([compiler, "install", str(PROJECT)], check=True, stdout=subprocess.DEVNULL)
    result = subprocess.run([compiler, "build", str(PROJECT), "--emit", "kicad_pcb", *SUPPLIER, "--json"],
                            text=True, stdout=subprocess.PIPE)
    if result.returncode:
        raise SystemExit(result.stdout or "CoHDL build failed; see stderr.")
    print(f"{version}: source build passed.")
    assembly = json.loads((PROJECT / "assembly.json").read_text())
    board_path = OUT / f"{NAME}.kicad_pcb"
    board = pcbnew.LoadBoard(str(board_path))
    footprints = {f.GetReference(): f for f in board.GetFootprints()}
    specified = set(assembly["dnp"]) | set(assembly["exclude_from_bom"]) | set(assembly["value_labels"])
    unknown = specified - footprints.keys()
    if unknown:
        raise SystemExit(f"Assembly metadata references missing parts: {sorted(unknown)}")
    excluded = set(assembly["exclude_from_bom"]) | set(assembly["dnp"])
    for ref, footprint in footprints.items():
        footprint.SetDNP(ref in assembly["dnp"])
        footprint.SetExcludedFromBOM(ref in excluded)
        footprint.SetExcludedFromPosFiles(ref in excluded)
        if ref in assembly["value_labels"]:
            footprint.SetValue(assembly["value_labels"][ref])
        if ref in assembly["dnp"]:
            footprint.SetValue(assembly["dnp"][ref]["value"])
            for pad in footprint.Pads():
                layers = pad.GetLayerSet()
                layers.RemoveLayer(pcbnew.F_Paste)
                layers.RemoveLayer(pcbnew.B_Paste)
                pad.SetLayerSet(layers)
    if not pcbnew.SaveBoard(str(board_path), board, True):
        raise SystemExit(f"Could not save generated board {board_path}")

    # Preserve the raw compiler BOM for inspecting all sites, including DNP.
    raw_bom = OUT / f"{NAME}-bom.csv"
    with raw_bom.open(newline="") as stream:
        reader = csv.DictReader(stream)
        fields, rows = reader.fieldnames, list(reader)
    fitted_bom = OUT / f"{NAME}-fitted-bom.csv"
    with fitted_bom.open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=fields)
        writer.writeheader()
        for row in rows:
            refs = [r.strip() for r in row["Designator"].split(",") if r.strip() not in excluded]
            if refs:
                row["Designator"] = ",".join(refs)
                writer.writerow(row)
    print(f"Assembly metadata applied: {len(assembly['dnp'])} DNP sites; {fitted_bom.name} written.")


if __name__ == "__main__":
    build()
