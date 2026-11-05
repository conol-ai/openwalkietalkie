# Rev B CoHDL source

Entry point: [`main/src/main.cohdl`](main/src/main.cohdl), design
`OpenMicroDialRevB`. The project was copied from Rev A's
`pcb-release-bodyv6.13-20260923-r6/source/main`, with the dependency versions
and `cohdl.lock` retained. `design.lock` preserves the released reference
designators, including tombstones for removed J4, C51 and R33.

The source now includes the changes that had only existed in the PCB:

- BQ24073RGTR charger, USB500 straps, 3.6k R37, CHG pull-up and PA33 status.
- Three-pin Molex battery connector with NTC and DNP bench substitute R44.
- ST MP34DT05TR-A microphone and adjustable RT6150BGQW IO regulator with
  R42/R43 feedback divider, carried forward from the r6 PCB ECOs.
- AMP_ENABLE on PA01, CAL on PA00, TOUCH_RESET pull-down and TP_VCC link.
- RF DC block and matching network, with all five DNP sites recorded.
- Released 0402 MCU capacitors and exact design-BOM manufacturer/MPNs.
- All 142 component placements, mounting holes and the rounded Rev B outline.

The encoder board is unchanged; its source remains in the
[r6 encoder project](../../rev-a/pcb-release-bodyv6.13-20260923-r6/source/encoder/).

## Build and verify

Use CoHDL **0.9.0** and KiCad 10's Python with `pcbnew`. The script detects
KiCad's bundled Python on macOS, or uses `python3` elsewhere. Override either
executable with `COHDL_BIN` or `KICAD_PYTHON` as needed.

From the repository root:

```sh
./hardware/rev-b/check.sh
```

This compiles into the ignored `main/out/` directory and validates against
`../main/main-revb.kicad_pcb` and `../main/assembly/bom.csv`. It does not change
the routed PCB or fabrication package. A successful run checks every
connected/NC numbered pad, reference set, values, packages, population,
component positions/sides/rotations, primary pad centers/sizes, mounting holes,
outline and fitted manufacturer/MPNs. The report is `main/out/source-parity.json`;
the reviewed snapshot is [`../verification/source-parity.json`](../verification/source-parity.json).

The charge-status net is named `CHG_STATUS` in the restored source and
`power::CHG_STATUS` in the released PCB. The checker normalizes that name and
the local footprint namespace. It also permits the source's unconnected
SKTD frame labels ML/MR to correspond to the PCB's unnumbered custom lands;
connecting those lands fails validation.

For a compiler-only check:

```sh
cohdl check hardware/rev-b/source/main --json
```

## Assembly and layout boundary

CoHDL 0.9.0 does not express DNP population. [`main/assembly.json`](main/assembly.json)
records C52, C53, C58, L6 and R44 as DNP, and excludes test points/mounting
holes from assembly output. `build_source.py` applies those flags, removes
paste on DNP pads and sets human-readable value labels on the **generated**
board. It does not add parts or change any pin connections. The unselected
RF shunt parts are footprint placeholders, not fitted tuning values.

Use `main/out/openmicrodial-rev-b-fitted-bom.csv` for the fitted source BOM.
The compiler's raw `*-bom.csv` includes all sites, including DNP placeholders.
Running `cohdl build` directly bypasses the assembly step.

The generated PCB is an unrouted layout starting point. The released PCB
retains routing, four-layer stackup, ground planes, keepouts, manufacturing
rules, silkscreen and 3D models. Parity does not claim a byte-identical PCB,
full pad-shape/stencil equivalence or a fresh DRC result. In particular, the
charger EP paste windows and SKTD frame lands use equivalent overlapping
primitives in source. Use the existing released fabrication package to order.

For subsequent electrical changes, edit this source and the routed PCB,
update assembly metadata/BOM as appropriate, and rerun `check.sh`. The
historical setup/routing scripts are not the maintained circuit definition.

Local device references: [TI BQ24073 datasheet](https://www.ti.com/lit/ds/symlink/bq24073.pdf),
[ST MP34DT05-A datasheet](https://www.st.com/resource/en/datasheet/mp34dt05-a.pdf).
