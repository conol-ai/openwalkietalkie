#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Derive the Rev C routed board from the released Rev B board (stdlib only).

usage: make_board.py [--check]

Rev C does not move copper. This text-level edit of
../../rev-b/main/main-revb.kicad_pcb changes two things and nothing else:

1. U4's footprint frame. soic 0.3.0 turned the land SOIC8P127X790X216N a
   quarter turn in its own frame. The footprint angle becomes 90 and every
   child coordinate is re-expressed in the turned frame, so each pad, the
   courtyard and the pin-1 marks stay where they are on the board. The 3D
   model is turned back by the same quarter turn.
2. 3D model paths: ${KIPRJMOD}/../models/ -> ${KIPRJMOD}/../../rev-b/models/,
   so Rev C reuses Rev B's models instead of carrying a second copy.

Every other byte is Rev B's. --check verifies the committed Rev C board equals
this derivation instead of writing it.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT.parent / "rev-b/main/main-revb.kicad_pcb"
TARGET = ROOT / "main/main-revc.kicad_pcb"
REF = "U4"
OLD_AT, NEW_AT = "(at 32.6 14.2)", "(at 32.6 14.2 90)"
OLD_MODELS, NEW_MODELS = "${KIPRJMOD}/../models/", "${KIPRJMOD}/../../rev-b/models/"
# A point stored in a footprint at angle 0 as (x, y) is (-y, x) in the same
# footprint at KiCad angle 90 (board position = origin + (y', -x')).
POINT = re.compile(r"\((at|start|end|center) (-?[\d.]+) (-?[\d.]+)( -?[\d.]+)?\)")


def number(value):
    text = f"{value:.6f}".rstrip("0").rstrip(".")
    return "0" if text in ("-0", "") else text


def turn(match):
    kind, x, y, angle = match.group(1), float(match.group(2)), float(match.group(3)), match.group(4) or ""
    return f"({kind} {number(-y)} {number(x)}{angle})"


def derive():
    text = SOURCE.read_text()
    marker = f'(property "Reference" "{REF}"'
    if text.count(marker) != 1:
        raise SystemExit(f"Expected exactly one {REF} in {SOURCE.name}")
    start = text.rfind("\n\t(footprint ", 0, text.index(marker))
    end = text.index("\n\t(footprint ", text.index(marker))
    block = text[start:end]
    if block.count(OLD_AT) != 1 or '"soic-SOIC8P127X790X216N"' not in block:
        raise SystemExit(f"{REF} is not the Rev B footprint this script was written for")
    head, body = block.split(OLD_AT)
    body = POINT.sub(turn, body)
    model = "(rotate\n\t\t\t\t(xyz 0 0 0)"
    if body.count(model) != 1:
        raise SystemExit(f"{REF}: unexpected 3D model block")
    body = body.replace(model, "(rotate\n\t\t\t\t(xyz 0 0 -90)")
    text = text[:start] + head + NEW_AT + body + text[end:]
    if OLD_MODELS not in text:
        raise SystemExit("No model paths found")
    return text.replace(OLD_MODELS, NEW_MODELS)


def main():
    text = derive()
    if "--check" in sys.argv:
        if TARGET.read_text() != text:
            raise SystemExit(f"{TARGET.name} is not the Rev B board with the documented Rev C edits")
        print(f"{TARGET.name} equals the Rev B board with the U4 frame and model-path edits only.")
        return
    TARGET.write_text(text)
    print(f"wrote {TARGET.name}")


if __name__ == "__main__":
    main()
