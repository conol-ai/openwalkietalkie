#!/bin/sh
# SPDX-License-Identifier: MIT
# Fabrication plots from the Rev C board with kicad-cli, using the same
# options as ../../rev-b/scripts/tooling/export_fab.sh.
# usage: export_fab.sh [OUTDIR]   (default: ../build/fab, ignored by git)
#   OUTDIR/gerber   4 copper, 2 mask, 2 silkscreen, outline
#   OUTDIR/stencil  F/B paste
#   OUTDIR/drill    PTH + NPTH Excellon
#   OUTDIR/positions-kicad-all.csv
#   OUTDIR/main-revc-jlcpcb-gerber.zip   the 13 plot files, flat
# Zones are plotted as stored in the board file; nothing is refilled or saved.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
if [ -n "${KICAD_CLI:-}" ]; then
    kc=$KICAD_CLI
elif [ -x /Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli ]; then
    kc=/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli
else
    kc=kicad-cli
fi
board=$root/main/main-revc.kicad_pcb
out=${1:-$root/build/fab}
rm -rf "$out"
mkdir -p "$out/gerber" "$out/stencil" "$out/drill"
"$kc" pcb export gerbers --no-protel-ext \
    -l F.Cu,In1.Cu,In2.Cu,B.Cu,F.Mask,B.Mask,F.Silkscreen,B.Silkscreen,Edge.Cuts -o "$out/gerber/" "$board"
"$kc" pcb export gerbers --no-protel-ext -l F.Paste,B.Paste -o "$out/stencil/" "$board"
"$kc" pcb export drill --format excellon --drill-origin absolute --excellon-zeros-format decimal \
    --excellon-oval-format route -u mm --excellon-separate-th -o "$out/drill/" "$board"
"$kc" pcb export pos --format csv --units mm --side both -o "$out/positions-kicad-all.csv" "$board"
(cd "$out" && zip -j -q -X main-revc-jlcpcb-gerber.zip gerber/*.gbr stencil/*.gbr drill/*.drl)
"$kc" version
