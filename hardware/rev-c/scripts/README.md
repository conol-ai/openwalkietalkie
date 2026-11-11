# Rev C scripts

Rev C did not lay out a board, so the layout history stays in
[`../../rev-b/scripts/`](../../rev-b/scripts/README.md). These are the
scripts that produce and check Rev C. All are MIT-licensed software.

| Script | Needs | What it does |
|---|---|---|
| `make_board.py` | Python 3 | Derives `main/main-revc.kicad_pcb` from Rev B's board by a text edit: U4's footprint frame and the 3D model paths. `--check` verifies the committed board is exactly that. |
| `pad_identity.py` | KiCad Python | Compares the Rev C and Rev B boards in the board frame: pads, tracks, vias, zones, graphics. |
| `build_source.py` | CoHDL, KiCad Python | `cohdl install` and `cohdl build` with the LCSC supplier selected, then applies `assembly.json` to the generated board and BOM. |
| `check_source.py` | KiCad Python | Source parity against the Rev C board, including the board-frame pad comparison. |
| `test_source_parity.py` | KiCad Python | Mutation tests for the parity check; run from this directory after `../check.sh`. |
| `build_fab.py` | KiCad Python | Writes the JLC BOM, CPL, orientation list, sourcing record and coverage report; `--check` compares them with the committed files. |
| `export_fab.sh` | kicad-cli | Plots Gerbers, paste, drill and positions into the ignored `build/fab/` and zips the 13 order files. |
| `compare_fab.py` | Python 3 | Compares the order zip with Rev B's plots, allowing only header metadata to differ. |

`../check.sh` runs everything except the tests, the export and DRC.

The order zip and `main/assembly/positions-kicad-all.csv` were produced with
`export_fab.sh` (KiCad 10.0.7) and copied from `build/fab/`. The DRC reports
in `../verification/` were produced with:

```sh
kicad-cli pcb drc --severity-all --refill-zones --format report --units mm \
    -o hardware/rev-c/verification/main-revc-drc.rpt hardware/rev-c/main/main-revc.kicad_pcb
```

and the same with `--format json`. The refill happens in memory; the board
file is not saved.
