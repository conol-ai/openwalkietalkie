# Rev C CoHDL source

Entry point: [`main/src/main.cohdl`](main/src/main.cohdl), design
`OpenMicroDialRevC`. The project is a copy of
[Rev B's source](../../rev-b/source/README.md) with:

- every dependency raised to the latest version published on
  registry.cohdl.org (`cohdl update`; twelve of seventeen moved) and
  `cohdl.lock` regenerated;
- one placement line changed: `place flash … rotate 180 side bottom` became
  `rotate 270 side bottom`, because `soic` 0.3.0 turned the SOIC-8 land a
  quarter turn in its own frame. U4's pads are where they were;
- the package and design renamed for Rev C. `design.lock` keeps every
  released reference designator and tombstone; only the design name in its
  keys changed.

The circuit is Rev B's. Rev B's source README describes what the source
contains; its design notes are in
[`../../rev-b/source/main/docs/`](../../rev-b/source/main/docs/).

## Build and verify

Use CoHDL **0.10.0** and KiCad 10's Python with `pcbnew`. Override either
executable with `COHDL_BIN` or `KICAD_PYTHON`. From the repository root:

```sh
./hardware/rev-c/check.sh
```

This compiles into the ignored `main/out/` directory and validates against
`../main/main-revc.kicad_pcb`, `../main/assembly/bom.csv` and the committed
order files in `../fab-jlcpcb/`. The steps are listed in the
[Rev C README](../README.md#build-and-verify). The reviewed snapshots are
[`../verification/source-parity.json`](../verification/source-parity.json)
and [`../verification/fab-coverage.json`](../verification/fab-coverage.json).

The same normalizations as Rev B apply (`CHG_STATUS`, the SKTD frame lands,
the project-local footprint namespace, the mounting-hole name), plus one
allowance: R39's two pads are transposed on the routed board (see the Rev C
README, known limitation 4).

For a compiler-only check:

```sh
cohdl check hardware/rev-c/source/main --json
```

## Supplier data

`cohdl build --supplier lcsc --bottom-rotation lcsc=mirrored` writes a
supplier BOM, CPL and map into `main/out/`. Three committed files turn them
into the order package:

- [`main/assembly.json`](main/assembly.json): DNP sites and the parts left
  out of assembly, as in Rev B. The compiler lists all of them in its
  supplier outputs; `build_fab.py` takes them out.
- [`main/supplier-fallback.csv`](main/supplier-fallback.csv): designator →
  LCSC number for every fitted placement the library does not bind, with
  the substitution and note text, copied from Rev B's order BOM. A
  designator the library binds must not be in this table, and a fitted,
  unbound designator must be; either mistake stops the build. When a library
  release binds one of these parts, delete its rows here.
- [`main/frame-evidence.json`](main/frame-evidence.json): the frame-evidence
  rows of the bindings in use, copied from the CoHDL library repository. The
  compiler output does not carry these ids, so the build joins them by part
  number, footprint and package version, and stops if a bound placement has
  no row or the row's map is not the binding's.

To regenerate the order files after a change, run
`scripts/build_fab.py` without `--check` (after `scripts/build_source.py`).

## Layout boundary

As in Rev B, the generated PCB is an unrouted starting point and the routed
board is the fabrication authority. For an electrical change, edit this
source and the routed board, update the assembly metadata and design BOM,
and rerun `check.sh`.
