#!/bin/sh
# SPDX-License-Identifier: MIT
# Rev C gate: the board is Rev B's copper, the source builds to that board,
# and the committed order data is what the source and library produce.
# Needs CoHDL (COHDL_BIN, version in .cohdl-version) and KiCad's Python
# (KICAD_PYTHON). Writes only under source/main/out/.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [ -n "${KICAD_PYTHON:-}" ]; then
    kpython=$KICAD_PYTHON
elif [ -x /Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/Current/bin/python3 ]; then
    kpython=/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/Current/bin/python3
else
    kpython=python3
fi
"$kpython" "$root/scripts/make_board.py" --check
"$kpython" "$root/scripts/pad_identity.py" --report "$root/source/main/out/copper-identity-vs-rev-b.json"
"$kpython" "$root/scripts/build_source.py"
"$kpython" "$root/scripts/check_source.py" --report "$root/source/main/out/source-parity.json"
"$kpython" "$root/scripts/build_fab.py" --check
"$kpython" "$root/scripts/compare_fab.py"
# The placement-preview files, when CoHDL's measure_preview.py is at hand.
if [ -n "${COHDL_MEASURE_PREVIEW:-}" ]; then
    "$kpython" "$root/scripts/build_preview.py" --measure-preview "$COHDL_MEASURE_PREVIEW" --check
fi
