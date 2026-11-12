# OpenWalkieTalkie Rev C main board (BodyV6.13 case)

Rev C is the [Rev B](../rev-b/README.md) main board re-expressed on the
current CoHDL compiler and library, with the JLCPCB BOM and placement file
produced through CoHDL supplier bindings. It was made at the maintainer's
request: "可以新作一份rev c，把包的版本和cohdl的版本都升级上去" (make a Rev C with the
package versions and the CoHDL version raised).

**Nothing electrical or physical changed.** The copper, drills, outline,
silkscreen and paste are Rev B's; a board made from this directory is the
same board as one made from `../rev-b`. What changed is how the design is
written down and where the order data comes from.

Status (2026-11-11):

| Check | Result |
|---|---|
| Rev C board vs Rev B board, in the board frame | **142 footprints, 493 pads, 1351 tracks, 477 vias, 36 zones identical**; one footprint frame turned (U4, 0° → 90°) |
| KiCad 10.0.7 DRC, zones refilled, all severities, Rev B's rules | **0 violations, 0 unconnected** |
| Gerber, paste and drill files vs Rev B's | 13 files, **identical apart from 50 header lines** (KiCad version, plot time, project name) |
| Source parity (`check.sh`) | passes: 142 footprints, 459 numbered pads, 454 pads compared in the board frame |
| Supplier-binding coverage | **6 of 124** LCSC-sourced placements come from library bindings; 118 from the fallback table |

## What changed against Rev B

| | Rev B | Rev C |
|---|---|---|
| Compiler | CoHDL 0.9.0 | CoHDL 0.10.0 (see [Compiler](#compiler)) |
| Library packages | pinned to the r6-era versions | latest versions published on registry.cohdl.org on 2026-11-11 (table below) |
| Source | `place flash … rotate 180 side bottom` | `rotate 270 side bottom`: `soic` 0.3.0 turned the SOIC-8 land a quarter turn in its own frame, so the placement angle changes to keep the pads where they are. Design renamed `OpenMicroDialRevC`; no other source line changed |
| Routed board | `main-revb.kicad_pcb` | `main-revc.kicad_pcb`: U4's footprint frame turned to match the source (angle 0° → 90°, children re-expressed so nothing moves), and 3D model paths pointed at `../rev-b/models`. 147 changed lines in total, produced by [`scripts/make_board.py`](scripts/make_board.py) |
| Design BOM | U1 manufacturer "Nsiway" | "NSIWAY" (spelling in `@contrib/audio-amp` 0.2.0); otherwise identical |
| JLC BOM / CPL | from the board by formula, LCSC numbers from a hand-kept table | from `cohdl build --supplier lcsc --bottom-rotation lcsc=mirrored` where the library binds the part, from a committed fallback table elsewhere; every row says which |
| CPL rows that differ | U4 rotation 180, J5 at (28.000, −4.000) | U4 rotation 90, J5 at (28.000, −5.550); see [ORDER-NOTES](fab-jlcpcb/ORDER-NOTES.md) |

The board's silkscreen still reads "OpenWalkieTalkie main Rev B / 2026-10" and
the title block still says revision B. Changing either would change a
Gerber, and Rev C's claim is that the Gerbers are Rev B's.

### Package versions

All from `https://registry.cohdl.org`; `source/main/cohdl.lock` records the
hashes. Written by `cohdl update`.

| Package | Rev B | Rev C |
|---|---|---|
| `@contrib/audio-amp` | 0.1.0 | 0.2.0 |
| `@contrib/display` | 0.1.0 | 0.1.0 |
| `@richtek/dcdc` | 0.1.2 | 0.1.4 |
| `@sifli/sf32` | 0.1.2 | 0.1.2 |
| `antenna` | 0.1.0 | 0.1.0 |
| `esd` | 0.1.1 | 0.1.2 |
| `flash` | 0.1.4 | 0.3.1 |
| `load-switch` | 0.1.1 | 0.2.0 |
| `mic` | 0.1.1 | 0.1.2 |
| `misc` | 0.1.1 | 0.1.1 |
| `osc` | 0.1.1 | 0.2.0 |
| `passive` | 0.2.2 | 0.2.6 |
| `qfn` | 0.2.0 | 0.3.0 |
| `soic` | 0.2.0 | 0.3.0 |
| `std` | 0.3.0 | 0.3.0 |
| `switches` | 0.1.2 | 0.1.3 |
| `usb` | 0.1.1 | 0.1.6 |

Twelve of seventeen moved. Of every library land the board uses, only
`soic::SOIC8P127X790X216N` (U4) changed geometry.

### Compiler

The build reports `cohdl 0.10.0`. CoHDL 0.10.0 is a published release: tag
`v0.10.0` (commit `2ca26c9`, 2026-11-06) with a GitHub release of the same
name. The binary used here was not the release download. It was built from
the CoHDL repository's `main` at commit `43c055e` (2026-11-11), 147 commits
after the tag. None of those commits touches `src/`, `Cargo.toml` or
`Cargo.lock`, so the compiler source is the release's; whether the release
binary gives byte-identical output was not tested.

## Files

| What | Where |
|---|---|
| CoHDL source | [`source/main/src/main.cohdl`](source/main/src/main.cohdl); [build and checks](source/README.md) |
| Routed board (KiCad 10, fabrication authority) | [`main/main-revc.kicad_pcb`](main/main-revc.kicad_pcb) with `.kicad_pro` and `.kicad_dru` copied from Rev B |
| JLCPCB order package | [`fab-jlcpcb/`](fab-jlcpcb/ORDER-NOTES.md). **Read ORDER-NOTES first.** |
| Placement preview before ordering | [`preview/`](preview/README.md): what goes through JLCPCB's assembly preview so that orientations are measured first. Two previews read on 2026-11-12 ([first](preview/2026-11-12/README.md), [second](preview/2026-11-12-iteration-2/README.md)): all 118 previewed placements confirmed, 13 of them with a board-local mapping that is now in the order CPL; 7 parts could not be previewed. |
| Design BOM / KiCad positions | [`main/assembly/bom.csv`](main/assembly/bom.csv), [`main/assembly/positions-kicad-all.csv`](main/assembly/positions-kicad-all.csv) |
| Verification reports | [`verification/`](verification/) |
| Scripts | [`scripts/`](scripts/README.md) |

Not copied, because they are unchanged and Rev C would only duplicate them:

| What | Use Rev B's |
|---|---|
| 3D models (3.9 MB) | [`../rev-b/models/`](../rev-b/models/ATTRIBUTION.md); the Rev C board references them directly |
| Populated and fit STEP files (33 MB) | [`../rev-b/3d/`](../rev-b/3d/) |
| Plot directories (`gerber/`, `stencil/`, `drill/`) | [`../rev-b/main/`](../rev-b/main/); Rev C carries only the order zip, compared file by file in [`verification/fab-compare-vs-rev-b.json`](verification/fab-compare-vs-rev-b.json) |
| UART flash jig | [`../rev-b/jig/`](../rev-b/jig/README.md) |
| Audio crosstalk, placement, 3D fit, netlist-diff and review reports | [`../rev-b/verification/`](../rev-b/verification/); they describe the same copper |
| Design notes, layout history, Chinese assembly BOM | [`../rev-b/source/main/docs/`](../rev-b/source/main/docs/), [`../rev-b/scripts/`](../rev-b/scripts/README.md), `../rev-b/openwalkietalkie-revb-bom-zh.xlsx` |

Rev B's README remains the description of the board itself: what moved from
r6, the decisions, the known limitations and the firmware notes all apply
unchanged. Firmware for Rev C hardware is the `hw-b` build.

## Build and verify

Needs CoHDL 0.10.0 (`COHDL_BIN`) and KiCad 10's Python with `pcbnew`
(`KICAD_PYTHON`; found automatically on macOS). From the repository root:

```sh
./hardware/rev-c/check.sh
```

It runs, in order:

1. `make_board.py --check`: the Rev C board is the Rev B board plus the two
   documented edits, byte for byte.
2. `pad_identity.py`: every pad (number, net, centre, size as it lies on the
   board, shape, type, drill, layers), track, via, zone and graphic equals
   Rev B's. Footprint angles are reported, not compared.
3. `build_source.py`: `cohdl install`, then `cohdl build --emit kicad_pcb
   --supplier lcsc --bottom-rotation lcsc=mirrored`, then the population
   flags from `assembly.json`.
4. `check_source.py`: Rev B's parity check against the Rev C board, plus a
   comparison of every primary pad in the board frame.
5. `build_fab.py --check`: the committed BOM, CPL, orientation list and
   sourcing record are what the source and library produce now, and no DNP
   site, test point or mounting hole appears in them.
6. `compare_fab.py`: the order zip's 13 files equal Rev B's apart from
   header metadata.

It writes only under `source/main/out/`. DRC and the Gerber export are not
part of it; see [`scripts/README.md`](scripts/README.md).

### Why the board-frame pad check

Raising `soic` from 0.2.0 to 0.3.0 with the Rev B placement line compiles
with no error and no warning, and puts U4's eight pads a quarter turn away
from the routed copper. Only a comparison against the board catches it.
[`verification/unmigrated-u4-check.txt`](verification/unmigrated-u4-check.txt)
is `check.sh` run on that source: it fails on all eight pads. The check
compares where each pad's copper lies on the board, so it does not depend on
which angle a footprint reports.

## Known limitations

Rev B's [known limitations](../rev-b/README.md#known-limitations) all apply.
Specific to Rev C:

1. **118 of 124 LCSC numbers still come from a table**, not from the
   library. The published `passive` 0.2.6 binds no resistor, and the JLC
   Basic resistors this board orders (UniOhm 0402WGF, 43 placements) have no
   library binding at all yet. The six bound placements are C12, C19, C20,
   C22, J5 and U4.
2. **Only two CPL rows carry a checked frame.** U4 and J5 come out of the
   library binding and differ from Rev B's file. The other 123 rows are the
   same uncorrected convention values as Rev B (119 of them for unbound
   parts, 4 for bound capacitors whose map is zero). Two placement previews
   on 2026-11-12 ([first](preview/2026-11-12/README.md),
   [second](preview/2026-11-12-iteration-2/README.md)) read 118 rows. 105
   were right; 13 were wrong (U1, U2, U10, U20, U7, U8, U3, J1, J3, SW1,
   SW2, SW5, SW6) and now carry a board-local mapping from
   [`source/main/placement-evidence.csv`](source/main/placement-evidence.csv),
   read right with it applied. Seven rows (U5, Y1, AE1, R32, SW3, SW4, MK1)
   were not read at all.
3. **The library evidence for U4 and J5 was taken on other boards**
   (preview records dated 2026-10-30). The 2026-11-12 preview confirmed
   both on this board. Neither has been compared with what was actually
   placed on Rev B boards; see the open questions.
4. **R39's pads.** On the routed board R39's two pads lie 0.64 × 0.54 mm;
   the `passive::CHIP_0402` land is 0.54 × 0.64 mm. The centres agree. This
   is on the Rev B board already (its check compared nominal pad sizes and
   did not see it). Rev C keeps the copper and the library part, and
   `check_source.py` allows exactly this transposition on exactly these two
   pads. Making the source agree would mean a design-local land for R39,
   which would take it off the land future library resistor bindings are
   measured on.
5. **Pad-shape equivalence in the source** is as in Rev B: the SKTD switch
   frame lands and the charger's paste windows are built from overlapping
   primitives, and pads that repeat a number are not shape-audited.
6. **DRC was run with KiCad 10.0.7**; Rev B's report is from 10.0.4. Rev B's
   board gives the same 0 / 0 under 10.0.7.
7. **U4's 3D model** was turned back with its frame. Bottom-side renders of
   both boards match; the model is symmetric under a half turn, so the
   render cannot tell −90° from +90°.
8. **CoHDL has no population concept.** `cohdl build --supplier` lists DNP
   sites and test points in its BOM and CPL. `build_fab.py` removes them
   using `assembly.json` and fails if one reaches an order file.

## Open questions for the maintainer

1. **Compiler.** Is released 0.10.0 the version Rev C should pin? The
   binary used was built from `main` at `43c055e`; its compiler source
   equals the `v0.10.0` tag.
2. **As-ordered files.** The Rev B boards were ordered with the BOM and
   placement file edited in JLC's interface (stock substitutions and
   coordinates), so neither Rev B's committed files nor these are what was
   placed. Could the as-ordered BOM and CPL be exported from JLC's order
   history? They would replace the guesswork in ORDER-NOTES, and show
   whether the U4 and J5 values here match the corrections made by hand.
3. **Wait for resistor bindings?** `passive` with the Yageo RC0402
   bindings is not published yet, and bindings for the UniOhm parts this
   board actually orders do not exist. Should Rev C merge now at 6 of 124,
   or wait until more of the BOM can come from the library?
4. **Board marking.** Should a later Rev C change the silkscreen and title
   block to say Rev C? It would change one silkscreen Gerber.
5. **R39.** Leave the transposed pads as they are (current choice), or fix
   the copper in a revision that changes copper anyway?

## License

As for Rev B: the design files here are CERN-OHL-P-2.0, see
[`../LICENSE`](../LICENSE) and [`../README.md`](../README.md). Everything
under `scripts/`, and `check.sh`, is software under the repository's MIT
license. Third-party models keep their own licenses, see
`../rev-b/models/ATTRIBUTION.md`.
