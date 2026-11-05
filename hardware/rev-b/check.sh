#!/bin/sh
# SPDX-License-Identifier: MIT
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [ -n "${KICAD_PYTHON:-}" ]; then
    kpython=$KICAD_PYTHON
elif [ -x /Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/Current/bin/python3 ]; then
    kpython=/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/Current/bin/python3
else
    kpython=python3
fi
"$kpython" "$root/scripts/build_source.py"
"$kpython" "$root/scripts/check_source.py" --report "$root/source/main/out/source-parity.json"
