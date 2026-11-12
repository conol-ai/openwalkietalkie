# Rev C placement preview, 2026-11-12, second iteration

The second reading of Rev C's placements in JLCPCB's assembly preview. It
tests the nine placements the [first reading](../2026-11-12/README.md) found
wrong, with their mapping applied, and reads the other 109 again. With the
owner's approval, preview package v3 was run through CoHDL's
`tools/frame_boards/jlc_preview.py` (conol-ai/cohdl#116, commit 1fe7273) on
2026-11-12. The run stopped at Component Placements. Nothing was saved to
the cart, quoted or ordered.

| | |
|---|---|
| Package | `owt-revc-preview.zip`, SHA-256 `2c2e91719bf24db6b924f28a3a2769ab831bb19756dd5d5656f17c0398eef9dd`, built at commit 343bd21 |
| Its members | [`package/SHA256SUMS`](package/SHA256SUMS), as in the ZIP; its corrections table, CPL, BOM and `board.json` beside it, byte for byte |
| Top export | [`exports/top.png`](exports/top.png), SHA-256 `cfce07fe671314aa24543d142407065c5f8b56a5a3572d8a4b464f64f33f96f5` |
| Bottom export | [`exports/bottom.png`](exports/bottom.png), SHA-256 `9c9226078416dba012366327682a37dc633b979f112cdeea370c07135dc11ff0` |
| What JLCPCB detected | 4 layers, 92.38 × 53.38 mm; 41 part numbers, all confirmed; 59 top and 59 bottom designators; no stock warning |
| PCBA type | **Standard** |
| Previewed | 118 placements. U5, Y1, AE1, R32, SW3, SW4 and MK1 were left out again and are **still not measured**. |

## Result

**All nine placements under test read right with their mapping applied, and
the other 109 read as they did in the first iteration.** Every one of the
118 previewed placements is now confirmed. [`placements.csv`](placements.csv)
has each one.

### The nine under test

Positions are in the footprint's own frame, in millimetres.

| Part | CPL row, iteration 1 → this one | Mapping | Reading | Verdict |
|---|---|---|---|---|
| U7, U8 (C147972) | rotation 90 → 0 | rotate 270 | pin 1 mark 353.0° and 352.8° from pad 1, 0.40 mm beyond it along the pad; terminal rows on our pad rows | CONFIRMED |
| U3 (C55247264) | rotation 180 → 0 | rotate 180 | pin 1 mark 4.4° from pad 1; mount ears on the MP pads; the 16-lead row on the 16-pad row, each lead on its pad, tips at y +1.76 (pads +1.03 to +1.83). The 15-pad row is under the actuator and not seen. | CONFIRMED |
| J1 (C177225) | (33.0000, −84.9000) 90 → (32.0500, −84.9000) 270 | rotate 180, offset (0, −0.95) | leads on pads 1–2: tips at y −2.62 (pads −1.6 to −3.2), 1.02 mm of each lead on its pad, centred across it within 0.012 mm; fitting nails centred at y +0.47 against the ML/MR pads at +0.5; pin 1 mark beside pad 1 | CONFIRMED |
| J3 (C293630) | (17.0000, −16.5000) 0 → (17.0000, −15.3500) 0 | offset (0, −1.15) | leads on pads 1–3: tips at y −2.64, 1.04 mm on the pad, centred across within 0.006 mm; fitting nails at y +0.49 against +0.5; pin 1 mark on the pin 1 side | CONFIRMED |
| SW1, SW2, SW6 (C115365) | x 3.7600, 270 → x 4.3100, 90 | rotate 180, offset (0, −0.55) | by eye: actuator outboard (y +0.76 to +1.68), 0.45 mm over the board edge; frame centred across within 0.01 mm; terminals on pads 1 and 2, at y −1.04 to −1.37 against pad centres at −1.35 | CONFIRMED |
| SW5 (C115365) | x 52.2400, 90 → x 51.6900, 270 | the same | the same (actuator y +0.74 to +1.65) | CONFIRMED |

No row needs a refined mapping. What the three offsets rest on:

- **J1, J3.** Two things agree: the fitting nails lie on the ML/MR pad
  centres within 0.03 mm, and each lead lies on its pad for 1.0 mm of the
  pad's 1.6 mm, ending 0.56 to 0.58 mm short of the pad's outer end. Along
  the lead the preview cannot say better than a few tenths of a millimetre
  where the manufacturer puts the lead on the land; across it the leads are
  centred.
- **The switches.** No pin 1 mark; the reading is the model's shape. The
  terminals sit 0.15 mm inboard of the pad centres, well inside the pads
  (y −0.8 to −1.9). The centre frame pad MC has nothing of the model on it
  but the frame's edge.

### Regression: the 109 placed as before

Each has the same CPL row as in iteration 1.

- U1 5.9°, U2 357.5°, U10 358.9°, U20 11.7°, U4 355.6°, U6 4.2°: the same
  mark positions and angles, to the digit. J5's pegs: (−0.05, +0.14) and
  (−0.04, +0.14) mm from their holes, the same. J2: the same.
- 96 chip bodies: all along their pads, at most 0.7° and 0.028 mm, as
  before. 87 readings are identical; nine (C8, C18, C23, C31, C40, C42,
  C43, C45, C46) moved by at most 0.2° and 0.002 mm.
- L1–L5: identical body readings.
- Between the two pairs of exports, pixels that differ by more than 24
  levels lie only at the nine re-placed parts and their neighbours.

### What the tool reported, and why

"10 with a pin 1 mark, 96 chip bodies along their pads; unverified: none;
manual review: J3, L1, L2, L3, SW1, SW2, SW5, SW6, U1, U2, U7, U8"
([`checks.json`](checks.json), [`run.log`](run.log)). Against iteration 1:

- **J3 lost its "mark" status because its mark moved with the model.** The
  tool looks for a mark within a reach of the footprint origin (farthest
  pad plus 1.2 mm: 5.03 mm for J3). The 1.15 mm offset carried the mark to
  5.40 mm from the origin, so no footprint claims it. The mark is there, on
  the pin 1 side.
- **L3 was in manual review in iteration 1 as well** (all of L1–L5 were: a
  two-terminal part that is not on a chip land has no evidence type in the
  tool). What changed is that **L4 and L5 left it, wrongly**: turned to
  their right place, U7's and U8's marks are now nearer to L4 and L5 (1.72
  against 1.91 mm) and the tool counts them for the inductors.
- U7 and U8 are in manual review for that reason; U1 and U2 because C1 and
  C5 take their marks, as in iteration 1. U3 and J1 now pass as "mark".

None of this changes a reading: `measure_preview.py measure` and
[`features.json`](features.json) give every mark with its position.

## How it was measured

As in the [first record](../2026-11-12/README.md#how-it-was-measured), with
the same `measure_preview.py` (CoHDL 1fe7273, body method 2) and the same
`board.json`. [`features.py`](features.py) differs from the first record's
in two ways: a mark's `owner` skips every two-terminal part, and `leads`
gives, for J1 and J3, how far each lead reaches along its pad and where it
lies across it. Calibration is the same: 39.396 px/mm in x, 39.381 in y; the
0.05 mm outline stroke biases a position by at most 0.025 mm.

[`crops/`](crops/) holds the same 22 crops, from these exports. The nine
under test were looked at; so were the rest, whose readings did not change.

[`SHA256SUMS`](SHA256SUMS) covers every other file in this directory.

## What follows from it

The 13 rows of the order CPL that the first record showed to be wrong (U1,
U2, U10, U20, U7, U8, U3, J1, J3, SW1, SW2, SW5, SW6) are now in
`../../fab-jlcpcb/main-cpl-jlcpcb.csv` with their mapping, from
`../../source/main/placement-evidence.csv`. Still unread: the seven parts
left out, the 15-pad row of U3, J2's contact row, and any bottom-side
offset (none of the mappings on the bottom has one).

## Licence

`crop.py` and `features.py` are MIT-licensed software. The exports are
JLCPCB's renderings of this board, kept as the evidence the readings rest on.
