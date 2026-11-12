# Rev C placement preview, 2026-11-12

The first reading of Rev C's placements in JLCPCB's assembly preview. With
the owner's approval, preview package v2 was run through CoHDL's
`tools/frame_boards/jlc_preview.py` (conol-ai/cohdl#116, commit 1fe7273) on
2026-11-12. The run stopped at Component Placements. Nothing was saved to
the cart, quoted or ordered.

| | |
|---|---|
| Package | `owt-revc-preview.zip`, SHA-256 `8254b63a3369b7d8adab818bc40975ccdda4b08b445f1494b4ef4f5f5d5d5847`, built at commit 9b3e7dd |
| Its members | [`package/SHA256SUMS`](package/SHA256SUMS), as in the ZIP |
| Top export | [`exports/top.png`](exports/top.png), SHA-256 `e66cfd404f5dbf3eca799423d844adf9cce89c91cff57db088b680d9ee622464` |
| Bottom export | [`exports/bottom.png`](exports/bottom.png), SHA-256 `1ca0b8ee6926c7e62fc4566e176e2b74a9cb79c2131020aca2eb03de785640a0` |
| What JLCPCB detected | 4 layers, 92.38 × 53.38 mm; 41 part numbers, all confirmed; 59 top and 59 bottom designators; no stock warning |
| PCBA type | **Standard.** Economic offers one assembly side on this four-layer board. |
| Previewed | 118 placements. Seven parts were left out and are **not measured**: U5, Y1, AE1, R32, SW3, SW4, MK1 ([`../left-out.csv`](../left-out.csv)). |

## Result

| Verdict | Placements | Part numbers |
|---|---|---|
| CONFIRMED: read right with the mapping this preview applied | 109 | 36 |
| NEEDS MAPPING: read wrong; the mapping it implies needs a second preview | 9 | 5 |
| UNREADABLE | 0 | 0 |
| NOT PREVIEWED | 7 | 6 and Y1 |

[`placements.csv`](placements.csv) has every placement, with its numbers.
[`part-numbers.csv`](part-numbers.csv) has one row per part number on a land.

### Parts that are not chip bodies

Bottom rows are read in the mirrored convention the CPL uses (rotation =
180 − KiCad angle): a mapping's rotate adds to the CPL rotation on both
sides, and on the bottom the rotate that cancels a reading equals the
reading, where on top it is 360 minus it.

| LCSC | Parts | Land | Mapping applied | Reading | Verdict |
|---|---|---|---|---|---|
| C189961 NS4150B | U1 | `contrib_audio_amp` SOP8P65X490X110N | rotate 270 (candidate) | pin 1 mark 5.9° from pad 1, on pad 1's lead | CONFIRMED |
| C131941 TPS22918DBVR | U2, U10 | `load_switch` FP_TI_DBV0006A | rotate 180 (candidate) | 357.5° and 358.9°; mark within 0.13 mm of pad 1's centre | CONFIRMED |
| C7519 USBLC6-2SC6 | U20 | `esd` FP_SOT_23_6 | rotate 270 (candidate) | 11.7°, on pad 1's lead | CONFIRMED |
| C97521 W25Q128JVSIQ | U4 | `soic` SOIC8P127X790X216N | library binding, rotate 0 | 355.6°, on pad 1's lead, lower left seen from the top | CONFIRMED |
| C165948 TYPE-C-31-M-12 | J5 | `usb` FP_USB_C_Receptacle_HRO_TYPE_C_31_M_12 | library binding, offset (0, −1.55) | pegs (−0.05, +0.14) and (−0.04, +0.14) mm from their holes; leads on the pad row | CONFIRMED |
| C15220 BQ24073RGTR | U6 | board-local VQFN16 | none | 4.2° | CONFIRMED at rotate 0 |
| C596732 FH34SRJ-4S | J2 | board-local | none | by eye: mount tabs on the MP pads within 0.15 mm; no pin 1 mark | CONFIRMED at rotate 0 (contact row not seen) |
| C16902 BLM31PG601SN1L | L1, L2 | board-local 1206 | none | body 0.0° mod 180, centred within 0.023 mm | CONFIRMED at rotate 0 |
| C97018 4.7 µH | L3 | `passive` FP_L_WPN201610U_2016Metric | rotate 0 | body 0.0°, 0.012 mm | CONFIRMED |
| C337893 2.2 µH | L4, L5 | `passive` FP_L_0806_2016Metric | rotate 0 | body 0.0°, within 0.013 mm | CONFIRMED |
| C147972 RT6150BGQW | U7, U8 | `qfn` QFN10N50P250X250_1EP120X200 | none | pin 1 mark 261.7° on both; the model's terminal rows lie across our pad rows | NEEDS MAPPING: **rotate 270** |
| C55247264 display FPC connector | U3 | board-local | none | pin 1 mark 184.8°; mount ears and the 16-lead row on the wrong side | NEEDS MAPPING: **rotate 180** |
| C177225 Molex 53261-0271 | J1 | board-local | none | leads point away from pads 1–2; mark diagonally opposite pad 1 | NEEDS MAPPING: **rotate 180**, offset about (0, −0.95) |
| C293630 Molex 53261-0371 | J3 | board-local | none | rotation right; lead tips end 0.11 mm short of pads 1–3 | NEEDS MAPPING: rotate 0, **offset about (0, −1.15)** |
| C115365 SKTDLDE010 | SW1, SW2, SW5, SW6 | board-local | none | by eye: actuator points inboard, over pad MC; terminals on the ML/MR side | NEEDS MAPPING: **rotate 180**, offset about (0, −0.55) |

Offsets are in the footprint's own frame, in millimetres, as in
`corrections.csv`: where the assembler's origin has to sit relative to the
footprint origin.

The 26 chip part numbers (96 bodies, `passive` CHIP_0402, 0603, 0805 and
1206) all read along their pads: at most 0.7° off, mod 180, and centred
within 0.028 mm against limits of 0.127 to 0.250 mm. A chip body reading is
blind to a half turn, which does not matter for these parts. The board has
no diode, LED or polarised capacitor.

### The three offsets are estimates

J1, J3 and the four switches sit off their lands along one axis. This
preview shows that an offset is needed and in which direction, but the
amount is taken from the drawn model, not from leads on pads:

- J3: the model's fitting-nail strips are centred at local y +1.65 and the
  ML/MR pads at +0.5; moving one onto the other puts the lead tips at
  y −2.64, 0.24 mm past the pad centres. ±0.3 mm.
- J1: the same, on a model that is also a half turn off: strips at +1.44
  once turned, so −0.95. ±0.3 mm. J1 and J3 are different part numbers with
  different models.
- SW1, SW2, SW5, SW6: turned 180°, the model's terminals lie at y −0.47 to
  −0.82, at the inner end of pads 1 and 2 (y −0.8 to −1.9). −0.55 is the
  offset from the footprint origin to the centre of the land's pads, which
  centres the terminals near y −1.2 and puts the actuator tip at +1.63,
  inside the courtyard's +1.75 and 0.4 mm over the board edge. ±0.2 mm.
  This reading has no pin 1 mark behind it. It rests on which end of the
  model is the actuator.

The second preview, with these applied, reads the remainder directly.

### What the tool reported, and why

`jlc_preview.py` printed "9 with a pin 1 mark, 96 chip bodies along their
pads; unverified: none; manual review: L1, L2, L3, L4, L5, SW1, SW2, SW5,
SW6, U1, U2, U7, U8" ([`checks.json`](checks.json), [`run.log`](run.log)).

- **U1, U2, U7 and U8 do have a pin 1 mark.** The tool gives each mark to
  the nearest footprint within reach, chip parts included, and on this
  board a chip part is nearer: C1 for U1's mark (1.47 against 3.02 mm), C5
  for U2's (1.54 against 1.72), C42 for U7's and C45 for U8's (1.32
  against 1.90). U10, the same part number as U2, kept its mark by 0.01 mm
  (1.68 against C49's 1.69). `measure_preview.py measure` reads all five;
  [`features.json`](features.json) lists every mark with both attributions.
- **J2's "mark" is not one.** It is a 0.76 × 3.55 mm magenta rectangle of
  the model, 4190 px where a pin 1 mark is 179 to 745 px.
- **J1 and J3 passed as "mark"**, which says only that a mark is there. J1
  is a half turn off and both are off their pads.
- L1–L5 and the switches have no evidence type in the tool. The inductors
  are read here with the tool's body method; the switches by eye.

## How it was measured

```sh
sips -s format bmp exports/top.png --out top.bmp
sips -s format bmp exports/bottom.png --out bottom.bmp
python3 -I -B <cohdl>/tools/frame_boards/measure_preview.py measure package/board.json top.bmp bottom.bmp measurements.json
python3 -I -B <cohdl>/tools/frame_boards/measure_preview.py pegs package/board.json top.bmp bottom.bmp J5
python3 -I -B features.py <cohdl>/tools/frame_boards/measure_preview.py package/board.json top.bmp bottom.bmp features.json
python3 -I -B crop.py <cohdl>/tools/frame_boards/measure_preview.py package/board.json top.bmp bottom.bmp crops J1 J2 ...
```

`measure_preview.py` is the one at CoHDL commit 1fe7273, with body method
2. [`measurements.json`](measurements.json) is its output,
[`pegs-J5.jsonl`](pegs-J5.jsonl) the peg reading, and
[`features.json`](features.json) the output of [`features.py`](features.py):
the calibration, every mark, the inductor bodies, and the white and dark
areas of each connector and switch model in the footprint's own frame.

Calibration: 39.396 px/mm in x and 39.381 in y on both exports, 0.04 %
apart. The outline is drawn with a 0.05 mm stroke where the tool assumes
0.1 mm, so the tool's frame is 0.025 mm too small on each side: a position
reads up to 0.025 mm toward the board centre, less away from the edges.
No verdict is that close: the largest chip offset is 0.028 mm against a
limit of 0.127 mm. Pegs and holes go through the same mapping.

[`crops/`](crops/) holds a crop of each of the 22 placements that is not a
chip part, written by [`crop.py`](crop.py): the export's pixels doubled,
with a cyan ring on our pad 1, a yellow square on each other numbered pad
and a white cross on the footprint origin. Bottom crops are the view from
below, as exported. All 22 were looked at; the "by eye" entries in
`placements.csv` come from them.

`run.log` is the tool's output with the run directory's path removed. Four
earlier runs the same day stopped earlier: three waiting for Both Sides,
which is disabled on the Economic PCBA type, and one at the BOM step
(C2917717, C327334 and C2799946 short, C2688664 matched to no part; hence
[`../left-out.csv`](../left-out.csv)).

## Rows of the committed CPL this shows to be wrong

`../../fab-jlcpcb/main-cpl-jlcpcb.csv`, which for these rows is also Rev
B's CPL (`../../../rev-b/`):

| Part | Committed row | Evidence says | State |
|---|---|---|---|
| U1 | rotation 270 | 180 | confirmed with the mapping applied |
| U2 | 270 | 90 | confirmed |
| U10 | 90 | 270 | confirmed |
| U20 | 270 | 180 | confirmed |
| U7, U8 | 90 | 0 | read; second preview needed |
| U3 | 180 | 0 | read; second preview needed |
| J1 | (33.0000, −84.9000), 90 | about (32.0500, −84.9000), 270 | read; second preview needed |
| J3 | (17.0000, −16.5000), 0 | about (17.0000, −15.3500), 0 | read; second preview needed |
| SW1, SW2, SW6 | x 3.7600, 270 | about x 4.3100, 90 | read by eye; second preview needed |
| SW5 | x 52.2400, 90 | about x 51.6900, 270 | read by eye; second preview needed |

Rev B's own CPL is also wrong for U4 (180; 90 is confirmed) and J5
(y −4.0000; −5.5500 is confirmed). Rev C's fab CPL already has both. Every
other previewed row is right as committed. The fab CPL is not changed here.

## Next

[`../corrections.csv`](../corrections.csv) is now the table for a second
preview: the 36 confirmed part numbers as they were, and the five that need
a mapping with the mapping this record implies, marked "under test". The
table this preview's package applied is
[`package/corrections.csv`](package/corrections.csv), byte for byte the
member of the ZIP. Whether a second package has been run is stated in
[`../README.md`](../README.md), not here. (It has:
[`../2026-11-12-iteration-2/`](../2026-11-12-iteration-2/README.md).)

[`SHA256SUMS`](SHA256SUMS) covers every other file in this directory.

## Licence

`crop.py` and `features.py` are MIT-licensed software. The exports are
JLCPCB's renderings of this board, kept as the evidence the readings rest on.
