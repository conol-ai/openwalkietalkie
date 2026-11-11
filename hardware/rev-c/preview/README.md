# Rev C placement preview (before ordering)

The maintainer corrected part orientations by hand in JLCPCB's interface when
earlier revisions were ordered, and those corrections cannot be exported. So
before anyone orders Rev C, the real board goes through JLCPCB's assembly
preview, stopped at Component Placements, and every part's orientation is
read from the preview's own exports. The readings then become library
bindings or local mappings, the board is previewed again, and a person
orders.

Uploading a package is the owner's decision, per package, by its SHA-256.
The preview never saves to the cart, quotes or orders.

## Status: first preview read on 2026-11-12; second package defined, not uploaded

Package v2 (SHA-256 `8254b63a…5847`, built at commit 9b3e7dd) went through
the preview on 2026-11-12 with the owner's approval and stopped at Component
Placements. The record, with both exports, every measurement and the crops,
is [`2026-11-12/`](2026-11-12/README.md).

- **109 of the 118 previewed placements are confirmed**, 36 part numbers:
  all 96 chip bodies, the five inductors and ferrites, U6, J2, the library
  bindings for U4 and J5, and the three candidate rotations (U20 rotate 270,
  U2/U10 rotate 180, U1 rotate 270) with the mapping applied.
- **Nine placements, five part numbers, read wrong** and need a mapping this
  preview did not carry: U7/U8 rotate 270, U3 rotate 180, J1 rotate 180 with
  an offset, J3 an offset, SW1/SW2/SW5/SW6 rotate 180 with an offset. The
  rotations are read; the three offsets are estimates.
- **Seven parts were not previewed** and stay unmeasured: U5, Y1, AE1, R32,
  SW3, SW4, MK1.

(Earlier runs the same day used the first package, with AE1, R32, SW3, SW4
and MK1 still in it, and stopped at or before the BOM step.)

**Second iteration (package v3): defined here, not uploaded.**
[`corrections.csv`](corrections.csv), [`manifest.json`](manifest.json) and
[`CHECKLIST.md`](CHECKLIST.md) now describe the package for a second
preview. It tests the nine placements that read wrong, with their mapping
applied:

| LCSC | Parts | Mapping under test | CPL row in this package (fab row) | Look at |
|---|---|---|---|---|
| C147972 | U7, U8 | rotate 270 | rotation 0 (90) | pin 1 mark at pad 1 |
| C55247264 | U3 | rotate 180 | rotation 0 (180) | mount ears and the 16-lead row on their pads |
| C177225 | J1 | rotate 180, offset (0, −0.95) | (32.0500, −84.9000) 270 ((33.0000, −84.9000) 90) | leads on pads 1–2 |
| C293630 | J3 | offset (0, −1.15) | (17.0000, −15.3500) 0 ((17.0000, −16.5000) 0) | leads on pads 1–3 |
| C115365 | SW1, SW2, SW6 | rotate 180, offset (0, −0.55) | x 4.3100, 90 (x 3.7600, 270) | actuator outboard, terminals on pads 1 and 2 |
| C115365 | SW5 | the same | x 51.6900, 270 (x 52.2400, 90) | the same |

The other 109 placements are placed as on 2026-11-12, when they were
confirmed. The seven parts left out are still left out and still unmeasured.
The run needs the Standard PCBA type; `jlc_preview.py` selects it. Uploading
it needs the owner's approval for that package, by its SHA-256.

The table package v2 applied is kept, byte for byte, as
[`2026-11-12/package/corrections.csv`](2026-11-12/package/corrections.csv);
the last column of `corrections.csv` says what changed against it per row.
In short: 36 part numbers keep their mapping, now confirmed (the three IC
rotations among them: C7519 rotate 270, C131941 rotate 180, C189961 rotate
270); five get the mapping above.

### How a mapping becomes a CPL row

`build_preview.py` applies a mapping as CoHDL's supplier CPL does
(`compose_place` in the compiler, then the `lcsc=mirrored` bottom
convention). The offset is in the footprint's own frame, +y down as KiCad
draws the footprint. On the bottom its x is mirrored first; then it is
turned by the placement's rotation and added to the position; the mapping's
rotate does not turn the offset. The rotate adds to the CPL rotation on both
sides. The script checks this on every run against the one case read on a
board, J5's binding `at (0mm, −1.55mm)` on the top, which turns
(28.0000, −4.0000) into (28.0000, −5.5500), and against bottom-side cases
worked by hand from the compiler's rules. **No bottom-side offset has been
read on a board yet**; this package has none either (U3, U7 and U8 are
rotations only).

The fab CPL in `../fab-jlcpcb/` is unchanged, and 13 of its rows are now
known to be wrong as they stand
([the list](2026-11-12/README.md#rows-of-the-committed-cpl-this-shows-to-be-wrong)).

## What the package is

[`../scripts/build_preview.py`](../scripts/build_preview.py) writes one ZIP in
the form CoHDL's `tools/frame_boards/jlc_preview.py` reads:

| File | What |
|---|---|
| `owt-revc-gerbers.zip` | `../fab-jlcpcb/main-revc-jlcpcb-gerber.zip`, byte for byte |
| `owt-revc-lcsc-bom.csv` | the fab BOM without the rows left out, one line per LCSC number |
| `owt-revc-lcsc-cpl.csv` | the fab CPL without the rows left out, with [`corrections.csv`](corrections.csv) applied |
| `board.json` | `measure_preview.py board` of `../main/main-revc.kicad_pcb`, limited to the previewed placements |
| `CHECKLIST.md` | [the checklist](CHECKLIST.md): what was left out, the corrections, every placement |
| `verification.json`, `README.txt`, `SHA256SUMS`, and the three tables here | the record |

The package itself is not committed. [`manifest.json`](manifest.json) records
the SHA-256 of each file in it that does not depend on the commit, and what
JLCPCB should detect: 4 layers, 53.43 × 92.43 mm, 41 BOM lines, 118
placements (59 top, 59 bottom).

## What differs from the order package

1. **Seven parts are left out** ([`left-out.csv`](left-out.csv)), because they
   cannot pass the BOM step: U5 (SF32LB525UC6, C47355842, stock 0 when read
   on 2026-11-11) and Y1 (no LCSC number) from the start; and, after the first
   preview run on 2026-11-12 stopped at the BOM step, AE1 (C2917717), R32
   (C327334) and SW3/SW4 (C2799946), which showed a stock shortage, and MK1
   (C2688664), for which the BOM step matched no part. **A part that is left
   out is not drawn, so this preview does not measure these seven.** They need
   a later preview, once they are in stock or have a part number the assembler
   matches, or a reading by another route.

   That first run also showed that JLCPCB's Economic PCBA type offers one
   assembly side only for this four-layer board: assembling both sides needs
   the **Standard** PCBA type, for the preview and for an order alike.
2. **Eight part numbers are placed with a mapping that is not in the order
   CPL** ([`corrections.csv`](corrections.csv)). Three were confirmed on
   2026-11-12 with the mapping applied: C7519 (U20) rotate 270, C131941
   (U2, U10) rotate 180, C189961 (U1) rotate 270. Five are under test in
   the second package; see the table above.
3. The six library-bound rows (C12, C19, C20, C22, J5, U4) are as in the
   order CPL. Every other row is at its convention value, which the
   2026-11-12 preview confirmed.
4. The BOM has one line per LCSC number (the order BOM has C23733, C25741
   and C17168 on more than one line).

DNP sites, test points and mounting holes are absent, as in the order
package. Bottom rotations are in the same convention (`lcsc=mirrored`:
180 − KiCad angle), in which a mapping's rotation adds to the CPL angle on
both sides.

[`stock-risks.csv`](stock-risks.csv) lists parts that are kept but may stop
the run at the BOM step: the preview tool stops on any stock warning. If one
is short, move it to `left-out.csv` and rebuild; it then goes unmeasured too.

## What the tool will and will not judge

`jlc_preview.py` checks a chip part (a `passive-CHIP_*` land with two pads)
by its drawn body and every other part by a pin 1 mark of its own. On this
board:

- 97 chip parts are checked by body.
- 10 ICs (MK1, U1–U4, U6–U8, U10, U20) should carry a pin 1 mark;
  `measure_preview.py measure` gives each mark's angle against pad 1.
- 16 parts have no evidence type in the tool and will come out as "manual
  review": AE1, L1–L5, SW3, SW4 (two terminals, not a chip land, no mark)
  and J1, J2, J3, J5, SW1, SW2, SW5, SW6 (connectors and switches; a mark is
  not certain). Read these by eye from crops of the exports. J5's shell pegs
  can be compared with its holes by `measure_preview.py pegs`.

On 2026-11-12 the tool also sent U1, U2, U7 and U8 to "manual review",
though each has a pin 1 mark: it gives a mark to the nearest footprint in
reach, chip parts included, and a chip part was nearer. It counted a magenta
part of J2's model as a mark. `measure_preview.py measure` reads the marks
whatever the tool's status says; see the record.

**Use `measure_preview.py` from conol-ai/cohdl#116 or later.** This board's
outline is not a rectangle (the lower left of its bounding box is cut away)
and J5 overhangs the top edge at mid width. The earlier tool calibrated an
export from the green along the image's middle row and column, which on this
board gives a scale about a third short. #116 reads each edge of the board's
bounding box over the whole export. Even so, check the calibration (px/mm
equal in x and y) before trusting a reading. The outline here is drawn with
a 0.05 mm stroke where the tool assumes 0.1 mm: a 0.025 mm bias per side.

## Build

```sh
python3 hardware/rev-c/scripts/build_preview.py \
    --measure-preview <cohdl>/tools/frame_boards/measure_preview.py            # manifest and checklist
python3 hardware/rev-c/scripts/build_preview.py \
    --measure-preview <cohdl>/tools/frame_boards/measure_preview.py --out DIR  # the package, from a clean tree
```

`../check.sh` runs the `--check` form when `COHDL_MEASURE_PREVIEW` names
`measure_preview.py`. `build_preview.py --self-test` checks only the
mapping arithmetic and needs neither KiCad nor CoHDL.
