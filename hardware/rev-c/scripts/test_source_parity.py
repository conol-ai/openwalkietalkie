#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Regression checks for the parity gate; run after ../check.sh with KiCad Python."""
import csv
from pathlib import Path
import tempfile
import unittest

import pcbnew

from check_source import NAME, PROJECT, validate


class SourceParityTests(unittest.TestCase):
    def test_detects_electrical_and_assembly_drift(self):
        original = PROJECT / f"out/{NAME}.kicad_pcb"
        fitted_bom = PROJECT / f"out/{NAME}-fitted-bom.csv"
        self.assertTrue(validate(original, fitted_bom)["passed"])

        def wrong_pin(board):
            mcu = board.FindFootprintByReference("U5")
            pad = next(p for p in mcu.Pads() if p.GetNumber() == "67")
            pad.SetNet(board.FindNet("GND"))

        def connected_nc(board):
            mcu = board.FindFootprintByReference("U5")
            pad = next(p for p in mcu.Pads() if p.GetNumber() == "25")
            pad.SetNet(board.FindNet("AMP_ENABLE"))

        def turned_pad(board):
            pad = next(p for p in board.FindFootprintByReference("C12").Pads() if p.GetNumber() == "1")
            pad.SetOrientationDegrees(pad.GetOrientationDegrees() + 90)

        def turned_land(board):
            flash = board.FindFootprintByReference("U4")
            for pad in flash.Pads():   # the footprint keeps its angle; its pads turn about the origin
                pad.Rotate(flash.GetPosition(), pcbnew.EDA_ANGLE(90, pcbnew.DEGREES_T))

        mutations = [
            ("wrong MCU pin", wrong_pin, "U5.67: net"),
            ("connected NC", connected_nc, "U5.25: net"),
            ("missing part", lambda b: b.Delete(b.FindFootprintByReference("R48")), "References missing"),
            ("old charge current", lambda b: b.FindFootprintByReference("R37").SetValue("2.55kohm"), "R37: value"),
            ("fitted RF shunt", lambda b: b.FindFootprintByReference("C52").SetDNP(False), "C52: DNP"),
            # Same centre and same nominal size, turned in place: only the board-frame check sees it.
            ("pad turned in place", turned_pad, "C12.1: pad copper differs on the board"),
            # A land turned inside its own frame under an unchanged placement (soic 0.2.0 -> 0.3.0).
            ("land turned in its frame", turned_land, "U4.1: pad center/size differs"),
        ]
        with tempfile.TemporaryDirectory(prefix="revc-parity-") as directory:
            changed = Path(directory) / "changed.kicad_pcb"
            for label, mutate, expected in mutations:
                with self.subTest(label=label):
                    board = pcbnew.LoadBoard(str(original))
                    mutate(board)
                    self.assertTrue(pcbnew.SaveBoard(str(changed), board, True))
                    report = validate(changed, fitted_bom)
                    self.assertFalse(report["passed"])
                    self.assertTrue(any(expected in error for error in report["errors"]), report["errors"])

            # A same-value capacitor substitution must still fail exact BOM parity.
            with fitted_bom.open(newline="") as stream:
                reader = csv.DictReader(stream)
                fields, rows = reader.fieldnames, list(reader)
            row = next(row for row in rows if "C28" in row["Designator"].split(","))
            row["Comment"] = "WRONG-MPN"
            changed_bom = Path(directory) / "changed-bom.csv"
            with changed_bom.open("w", newline="") as stream:
                writer = csv.DictWriter(stream, fieldnames=fields)
                writer.writeheader()
                writer.writerows(rows)
            report = validate(original, changed_bom)
            self.assertFalse(report["passed"])
            self.assertTrue(any("C28: BOM" in error for error in report["errors"]))


if __name__ == "__main__":
    unittest.main()
