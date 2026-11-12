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

## Status: two previews read on 2026-11-12; all 118 previewed placements confirmed

| Package | Run | Record | Outcome |
|---|---|---|---|
| v2, SHA-256 `8254b63a…5847`, built at 9b3e7dd | 2026-11-12 | [`2026-11-12/`](2026-11-12/README.md) | 109 placements confirmed; 9 read wrong (U7, U8, U3, J1, J3, SW1, SW2, SW5, SW6) |
| v3, SHA-256 `2c2e9171…f9dd`, built at 343bd21 | 2026-11-12 | [`2026-11-12-iteration-2/`](2026-11-12-iteration-2/README.md) | the nine read right with their mapping applied; the 109 read as before |

Both ran with the owner's approval and stopped at Component Placements.
(Earlier runs the same day used the first package, with AE1, R32, SW3, SW4
and MK1 still in it, and stopped at or before the BOM step.)

- **All 118 previewed placements are confirmed.** 13 of them needed a
  mapping: U20 rotate 270, U2/U10 rotate 180, U1 rotate 270, U7/U8 rotate
  270, U3 rotate 180, J1 rotate 180 with offset (0, −0.95), J3 offset
  (0, −1.15), SW1/SW2/SW5/SW6 rotate 180 with offset (0, −0.55).
- **The order CPL now carries those 13**, from
  [`../source/main/placement-evidence.csv`](../source/main/placement-evidence.csv),
  and equals package v3's CPL row for row. `build_preview.py` checks that.
- **Seven parts were not previewed** and stay unmeasured: U5, Y1, AE1, R32,
  SW3, SW4, MK1. They need a third preview once they can pass the BOM step.
  No third package is needed for anything else.

[`corrections.csv`](corrections.csv) and [`CHECKLIST.md`](CHECKLIST.md) are
still package v3's, the last package run: the five rows marked "under test"
are the ones the second record confirmed, and the checklist's "fab row" is
the order row as it was before the mapping moved into the order CPL.
[`manifest.json`](manifest.json) differs from v3's only in the hash of the
order CPL it starts from. No package has been built since, and nothing has
been uploaded since.

The table package v2 applied is kept, byte for byte, as
[`2026-11-12/package/corrections.csv`](2026-11-12/package/corrections.csv),
and v3's as
[`2026-11-12-iteration-2/package/corrections.csv`](2026-11-12-iteration-2/package/corrections.csv).

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

The 13 rows the first record showed to be wrong
([the list](2026-11-12/README.md#rows-of-the-committed-cpl-this-shows-to-be-wrong))
are corrected in the order CPL in `../fab-jlcpcb/`.

## What the package is

[`../scripts/build_preview.py`](../scripts/build_preview.py) writes one ZIP in
the form CoHDL's `tools/frame_boards/jlc_preview.py` reads:

| File | What |
|---|---|
| `owt-revc-gerbers.zip` | `../fab-jlcpcb/main-revc-jlcpcb-gerber.zip`, byte for byte |
| `owt-revc-lcsc-bom.csv` | the fab BOM without the rows left out, one line per LCSC number |
| `owt-revc-lcsc-cpl.csv` | the fab CPL without the rows left out, with [`corrections.csv`](corrections.csv) applied; a row that already carries a board-local mapping in the fab CPL starts from its footprint origin and convention angle, and must come out as the fab row |
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
