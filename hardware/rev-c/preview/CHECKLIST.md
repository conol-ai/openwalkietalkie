# OpenWalkieTalkie Rev C: placement-preview checklist

For JLCPCB's assembly preview only. Stop at Component Placements. Do not save to the cart, quote or order.
**Not uploaded.** An upload needs the owner's approval for this package, by its SHA-256.

Upload `owt-revc-gerbers.zip` as the Gerber, `owt-revc-lcsc-bom.csv` as the BOM and `owt-revc-lcsc-cpl.csv` as the CPL.
Expect 4 layers, 53.43 x 92.43 mm, 45 BOM lines and 123 placements: 60 top, 63 bottom. Assemble both sides.

## Left out, so not previewed

A part that is not in the BOM and CPL is not drawn. Its orientation stays unmeasured after this preview.

| Designator | LCSC | Why |
|---|---|---|
| U5 | C47355842 | SF32LB525UC6: LCSC stock 0 when read on 2026-11-11. The preview tool stops at the BOM step on any stock warning. |
| Y1 | none | E1SB48E001G00E 48 MHz crystal: no LCSC number, so the BOM step cannot match it. |

DNP sites (C52, C53, C58, L6, R44), test points and mounting holes are not assembled and are absent, as in the fab package.

## Stock risks, kept in

The preview tool stops at the BOM step on any stock warning. If one of these is short, move it to `left-out.csv` and rebuild.

| LCSC | Designators | Stock | Read on | Note |
|---|---|---|---|---|
| C147972 | U7, U8 | 50 | 2026-11-11 | RT6150BGQW; two per board. |
| C115365 | SW1, SW2, SW5, SW6 | 155 | 2026-11-11 | SKTDLDE010; four per board. |
| C2688664 | MK1 | 229 | 2026-11-11 | MP34DT05TR-A. |
| C15220 | U6 | 687 | 2026-11-11 | BQ24073RGTR. |
| C2917717 | AE1 | not read | n/a | 2450AT18B100E: out of stock at LCSC on 2026-09-23; not read again since. |
| C2799946 | SW3, SW4 | not read | n/a | PTS636SM25SMTRLFS: out of stock at LCSC on 2026-09-23; not read again since. |
| C327334 | R32 | not read | n/a | 604k resistor: out of stock at LCSC with low JLC stock in September; not read again since. |

## Corrections applied

Rows not listed here are at the convention value (top = KiCad angle, bottom = 180 - KiCad angle) with no evidence.

| LCSC | Board footprint | Rotate | Offset mm | Evidence | Detail |
|---|---|---|---|---|---|
| C15850 | `passive-CHIP_0805` | 0 | (0, 0) | library binding | Frame record c2b1-c0805-c15850; the fab CPL row already carries it. |
| C1525 | `passive-CHIP_0402` | 0 | (0, 0) | library binding | Frame record c2b1-c0402-c1525; the fab CPL row already carries it. |
| C165948 | `usb-connectors-type_c-FP_USB_C_Receptacle_HRO_TYPE_C_31_M_12` | 0 | (0, -1.55) | library binding | Frame record usbc-origin-c165948; the fab CPL row already carries it. |
| C97521 | `soic-SOIC8P127X790X216N` | 0 | (0, 0) | library binding | Frame record c2b1-soic8_208-c97521; read 0 with this mapping on CoHDL preview board 4 (2026-11-11). |
| C7519 | `esd-FP_SOT_23_6` | 270 | (0, 0) | board-4 raw reading | Pin 1 read 100.1-100.8 deg counter-clockwise of pad 1 on four copies placed with no mapping; implies rotate 270. Not yet confirmed with the mapping applied: this preview is that test. |
| C131941 | `load_switch-FP_TI_DBV0006A` | 180 | (0, 0) | board-4 raw reading | Pin 1 read 181.8-183.0 deg on four copies placed with no mapping; implies rotate 180. Not yet confirmed with the mapping applied: this preview is that test. |
| C189961 | `contrib_audio_amp-SOP8P65X490X110N` | 270 | (0, 0) | board-4 raw reading | Pin 1 read 94.7-95.2 deg on four copies placed with no mapping; implies rotate 270. Not yet confirmed with the mapping applied: this preview is that test. |
| C113780 | `passive-CHIP_0402` | 0 | (0, 0) | board-4 raw reading | Body along its pads (mod 180) and centred on four copies; nothing to apply. |
| C107006 | `passive-CHIP_0402` | 0 | (0, 0) | board-4 raw reading | Body along its pads (mod 180) and centred on four copies; nothing to apply. |
| C106255 | `passive-CHIP_0402` | 0 | (0, 0) | board-4 raw reading | Body along its pads (mod 180) and centred on four copies; nothing to apply. |
| C52923 | `passive-CHIP_0402` | 0 | (0, 0) | board-4 raw reading | Body along its pads (mod 180) and centred on four copies; nothing to apply. |
| C23733 | `passive-CHIP_0402` | 0 | (0, 0) | board-4 raw reading | Body along its pads (mod 180) and centred on four copies; nothing to apply. |
| C1548 | `passive-CHIP_0402` | 0 | (0, 0) | board-4 raw reading | Body along its pads (mod 180) and centred on four copies; nothing to apply. |
| C25741 | `passive-CHIP_0402` | 0 | (0, 0) | board-4 raw reading | Body along its pads (mod 180) and centred on four copies; nothing to apply. |
| C11702 | `passive-CHIP_0402` | 0 | (0, 0) | board-4 raw reading | Body along its pads (mod 180) and centred on four copies; nothing to apply. |
| C26083 | `passive-CHIP_0402` | 0 | (0, 0) | board-4 raw reading | Body along its pads (mod 180) and centred on four copies; nothing to apply. |
| C17168 | `passive-CHIP_0402` | 0 | (0, 0) | board-4 raw reading | Body along its pads (mod 180) and centred on four copies; nothing to apply. |
| C25768 | `passive-CHIP_0402` | 0 | (0, 0) | board-4 raw reading | Body along its pads (mod 180) and centred on four copies; nothing to apply. |
| C25905 | `passive-CHIP_0402` | 0 | (0, 0) | board-4 raw reading | Body along its pads (mod 180) and centred on four copies; nothing to apply. |
| C25092 | `passive-CHIP_0402` | 0 | (0, 0) | board-4 raw reading | Body along its pads (mod 180) and centred on four copies; nothing to apply. |
| C25890 | `passive-CHIP_0402` | 0 | (0, 0) | board-4 raw reading | Body along its pads (mod 180) and centred on four copies; nothing to apply. |
| C107039 | `passive-CHIP_0603` | 0 | (0, 0) | board-4 raw reading | Body along its pads (mod 180) and centred on four copies; nothing to apply. |
| C19666 | `passive-CHIP_0603` | 0 | (0, 0) | board-4 raw reading | Body along its pads (mod 180) and centred on four copies; nothing to apply. |
| C337893 | `passive-FP_L_0806_2016Metric` | 0 | (0, 0) | board-4 raw reading | Body along its pads (mod 180) and centred on four copies; nothing to apply. |
| C97018 | `passive-FP_L_WPN201610U_2016Metric` | 0 | (0, 0) | board-4 raw reading | Drawn as one block over both pads, long side along them, on four copies; nothing to apply. |

## What the tool checks, and what needs reading by eye

- `body`: a chip part; the tool reads its drawn body along its pads (mod 180) and centred.
- `pin 1 mark`: the tool needs one pin 1 mark on the part; `measure_preview.py measure` gives its angle against pad 1.
- `by eye`: the tool has no evidence type for it. Expect "manual review"; read it from a crop of the export.

- by eye (two-terminal, no mark): 8: AE1, L1, L2, L3, L4, L5, SW3, SW4
- body: 97: C1, C2, C3, C4, C5, C6, C7, C8, C9, C10, C11, C12, C13, C14, C15, C16, C17, C18, C19, C20, C21, C22, C23, C24, C25, C26, C27, C28, C29, C30, C31, C32, C33, C34, C35, C36, C37, C38, C39, C40, C41, C42, C43, C44, C45, C46, C47, C48, C49, C50, C57, R1, R2, R3, R4, R5, R6, R7, R8, R9, R10, R11, R12, R13, R14, R15, R16, R17, R18, R19, R20, R21, R22, R23, R24, R25, R26, R27, R28, R29, R30, R31, R32, R34, R35, R36, R37, R38, R39, R40, R41, R42, R43, R45, R46, R47, R48
- by eye (mark not certain): 8: J1, J2, J3, J5, SW1, SW2, SW5, SW6
- pin 1 mark: 10: MK1, U1, U2, U3, U4, U6, U7, U8, U10, U20

U4: pin 1 must land toward the lower left seen from the top. J5: the shell pegs must sit in their holes.
U1, U2, U10 and U20 are placed with the candidate rotation: the model's leads must lie on the pads and its pin 1 on pad 1.

## Placements

| Ref | Side | LCSC | Board footprint | Mid X | Mid Y | Convention | Preview rotation | Evidence | Applied | Check | Reading |
|---|---|---|---|---|---|---|---|---|---|---|---|
| AE1 | Top | C2917717 | `antenna-FP_ANT_2450AT18B100E` | 52.2000 | -24.6000 | 90 | 90 | none |  | by eye (two-terminal, no mark) | |
| C1 | Top | C15849 | `passive-CHIP_0603` | 43.5000 | -68.6000 | 0 | 0 | none |  | body | |
| C2 | Bottom | C107039 | `passive-CHIP_0603` | 46.6000 | -20.6500 | 90 | 90 | board-4 raw reading | rotate 0, nothing applied | body | |
| C3 | Top | C15849 | `passive-CHIP_0603` | 41.6500 | -65.8500 | 270 | 270 | none |  | body | |
| C4 | Top | C15849 | `passive-CHIP_0603` | 40.0500 | -65.8500 | 270 | 270 | none |  | body | |
| C5 | Bottom | C15849 | `passive-CHIP_0603` | 51.0000 | -60.6000 | 180 | 180 | none |  | body | |
| C6 | Bottom | C113780 | `passive-CHIP_0402` | 53.6000 | -64.9000 | 270 | 270 | board-4 raw reading | rotate 0, nothing applied | body | |
| C7 | Bottom | C97946 | `passive-CHIP_1206` | 48.0000 | -76.3000 | 180 | 180 | none |  | body | |
| C8 | Top | C107006 | `passive-CHIP_0402` | 45.4500 | -81.2000 | 0 | 0 | board-4 raw reading | rotate 0, nothing applied | body | |
| C9 | Top | C107006 | `passive-CHIP_0402` | 37.5500 | -81.2000 | 180 | 180 | board-4 raw reading | rotate 0, nothing applied | body | |
| C10 | Top | C107006 | `passive-CHIP_0402` | 41.5000 | -81.2000 | 90 | 90 | board-4 raw reading | rotate 0, nothing applied | body | |
| C11 | Top | C15849 | `passive-CHIP_0603` | 41.5000 | -76.4000 | 0 | 0 | none |  | body | |
| C12 | Bottom | C15850 | `passive-CHIP_0805` | 41.5000 | -79.4000 | binding | 180 | library binding | binding map rotate 0, offset (0, 0) mm, already in the fab row | body | |
| C13 | Bottom | C23733 | `passive-CHIP_0402` | 34.5000 | -28.9000 | 0 | 0 | board-4 raw reading | rotate 0, nothing applied | body | |
| C14 | Bottom | C107369 | `passive-CHIP_0402` | 43.8500 | -22.9500 | 180 | 180 | none |  | body | |
| C15 | Top | C52923 | `passive-CHIP_0402` | 43.9000 | -23.1500 | 0 | 0 | board-4 raw reading | rotate 0, nothing applied | body | |
| C16 | Bottom | C107369 | `passive-CHIP_0402` | 44.6000 | -28.0500 | 180 | 180 | none |  | body | |
| C17 | Bottom | C23733 | `passive-CHIP_0402` | 34.1500 | -20.4000 | 0 | 0 | board-4 raw reading | rotate 0, nothing applied | body | |
| C18 | Bottom | C19666 | `passive-CHIP_0603` | 36.7500 | -33.5500 | 180 | 180 | board-4 raw reading | rotate 0, nothing applied | body | |
| C19 | Bottom | C1525 | `passive-CHIP_0402` | 34.7500 | -33.3500 | binding | 90 | library binding | binding map rotate 0, offset (0, 0) mm, already in the fab row | body | |
| C20 | Bottom | C1525 | `passive-CHIP_0402` | 32.4500 | -33.3500 | binding | 90 | library binding | binding map rotate 0, offset (0, 0) mm, already in the fab row | body | |
| C21 | Bottom | C15849 | `passive-CHIP_0603` | 29.4000 | -38.7000 | 0 | 0 | none |  | body | |
| C22 | Bottom | C1525 | `passive-CHIP_0402` | 36.4500 | -18.2000 | binding | 270 | library binding | binding map rotate 0, offset (0, 0) mm, already in the fab row | body | |
| C23 | Top | C107369 | `passive-CHIP_0402` | 37.2000 | -17.5500 | 90 | 90 | none |  | body | |
| C24 | Bottom | C107369 | `passive-CHIP_0402` | 32.2000 | -22.6000 | 0 | 0 | none |  | body | |
| C25 | Bottom | C106255 | `passive-CHIP_0402` | 32.2000 | -23.6000 | 0 | 0 | board-4 raw reading | rotate 0, nothing applied | body | |
| C26 | Bottom | C52923 | `passive-CHIP_0402` | 32.2000 | -24.6000 | 0 | 0 | board-4 raw reading | rotate 0, nothing applied | body | |
| C27 | Top | C107369 | `passive-CHIP_0402` | 38.3000 | -17.5500 | 90 | 90 | none |  | body | |
| C28 | Top | C23733 | `passive-CHIP_0402` | 39.4000 | -17.5500 | 90 | 90 | board-4 raw reading | rotate 0, nothing applied | body | |
| C29 | Bottom | C23733 | `passive-CHIP_0402` | 34.1500 | -21.4000 | 0 | 0 | board-4 raw reading | rotate 0, nothing applied | body | |
| C30 | Bottom | C23733 | `passive-CHIP_0402` | 34.1500 | -22.4000 | 0 | 0 | board-4 raw reading | rotate 0, nothing applied | body | |
| C31 | Top | C107369 | `passive-CHIP_0402` | 36.1000 | -17.5500 | 90 | 90 | none |  | body | |
| C32 | Top | C113780 | `passive-CHIP_0402` | 49.5000 | -41.7500 | 270 | 270 | board-4 raw reading | rotate 0, nothing applied | body | |
| C33 | Top | C113780 | `passive-CHIP_0402` | 6.8000 | -17.0500 | 90 | 90 | board-4 raw reading | rotate 0, nothing applied | body | |
| C34 | Top | C113780 | `passive-CHIP_0402` | 6.8000 | -29.0500 | 90 | 90 | board-4 raw reading | rotate 0, nothing applied | body | |
| C35 | Top | C113780 | `passive-CHIP_0402` | 6.8000 | -41.0500 | 90 | 90 | board-4 raw reading | rotate 0, nothing applied | body | |
| C36 | Top | C113780 | `passive-CHIP_0402` | 34.6000 | -53.6000 | 270 | 270 | board-4 raw reading | rotate 0, nothing applied | body | |
| C37 | Top | C113780 | `passive-CHIP_0402` | 13.6500 | -53.6000 | 270 | 270 | board-4 raw reading | rotate 0, nothing applied | body | |
| C38 | Top | C113780 | `passive-CHIP_0402` | 26.8500 | -22.2500 | 0 | 0 | board-4 raw reading | rotate 0, nothing applied | body | |
| C39 | Top | C113780 | `passive-CHIP_0402` | 29.6000 | -22.2500 | 180 | 180 | board-4 raw reading | rotate 0, nothing applied | body | |
| C40 | Top | C19666 | `passive-CHIP_0603` | 11.1000 | -8.5500 | 180 | 180 | board-4 raw reading | rotate 0, nothing applied | body | |
| C41 | Top | C19666 | `passive-CHIP_0603` | 16.6000 | -5.1000 | 0 | 0 | board-4 raw reading | rotate 0, nothing applied | body | |
| C42 | Bottom | C342827 | `passive-CHIP_1206` | 12.0500 | -43.5000 | 90 | 90 | none |  | body | |
| C43 | Bottom | C342827 | `passive-CHIP_1206` | 17.9500 | -43.5000 | 90 | 90 | none |  | body | |
| C44 | Bottom | C342827 | `passive-CHIP_1206` | 20.4000 | -43.5000 | 90 | 90 | none |  | body | |
| C45 | Bottom | C342827 | `passive-CHIP_1206` | 19.0500 | -35.5000 | 90 | 90 | none |  | body | |
| C46 | Bottom | C342827 | `passive-CHIP_1206` | 24.9500 | -35.5000 | 90 | 90 | none |  | body | |
| C47 | Bottom | C342827 | `passive-CHIP_1206` | 27.4000 | -35.5000 | 90 | 90 | none |  | body | |
| C48 | Top | C19666 | `passive-CHIP_0603` | 18.5000 | -9.3500 | 270 | 270 | board-4 raw reading | rotate 0, nothing applied | body | |
| C49 | Bottom | C15849 | `passive-CHIP_0603` | 26.5000 | -46.0000 | 0 | 0 | none |  | body | |
| C50 | Bottom | C113780 | `passive-CHIP_0402` | 24.4500 | -40.3500 | 0 | 0 | board-4 raw reading | rotate 0, nothing applied | body | |
| C57 | Bottom | C1548 | `passive-CHIP_0402` | 43.4000 | -29.5500 | 270 | 270 | board-4 raw reading | rotate 0, nothing applied | body | |
| J1 | Top | C177225 | `openmicrodial_rev_a-FP_MOLEX_53261_0271` | 33.0000 | -84.9000 | 90 | 90 | none |  | by eye (mark not certain) | |
| J2 | Top | C596732 | `openmicrodial_rev_a-FP_HIROSE_FH34SRJ_4S` | 32.5000 | -16.8000 | 270 | 270 | none |  | by eye (mark not certain) | |
| J3 | Top | C293630 | `openmicrodial_rev_b-FP_MOLEX_53261_0371` | 17.0000 | -16.5000 | 0 | 0 | none |  | by eye (mark not certain) | |
| J5 | Top | C165948 | `usb-connectors-type_c-FP_USB_C_Receptacle_HRO_TYPE_C_31_M_12` | 28.0000 | -5.5500 | binding | 180 | library binding | binding map rotate 0, offset (0, -1.55) mm, already in the fab row | by eye (mark not certain) | |
| L1 | Top | C16902 | `openmicrodial_rev_a-FP_MURATA_BLM31PG_1P5A_REFLOW` | 43.3000 | -79.6500 | 270 | 270 | none |  | by eye (two-terminal, no mark) | |
| L2 | Top | C16902 | `openmicrodial_rev_a-FP_MURATA_BLM31PG_1P5A_REFLOW` | 39.7000 | -79.6500 | 270 | 270 | none |  | by eye (two-terminal, no mark) | |
| L3 | Bottom | C97018 | `passive-FP_L_WPN201610U_2016Metric` | 34.0000 | -18.7000 | 0 | 0 | board-4 raw reading | rotate 0, nothing applied | by eye (two-terminal, no mark) | |
| L4 | Bottom | C337893 | `passive-FP_L_0806_2016Metric` | 15.0000 | -45.5000 | 180 | 180 | board-4 raw reading | rotate 0, nothing applied | by eye (two-terminal, no mark) | |
| L5 | Bottom | C337893 | `passive-FP_L_0806_2016Metric` | 22.0000 | -37.5000 | 180 | 180 | board-4 raw reading | rotate 0, nothing applied | by eye (two-terminal, no mark) | |
| MK1 | Bottom | C2688664 | `openmicrodial_rev_a-FP_ST_MP34DT05_HCLGA_3x4` | 48.0000 | -70.5000 | 180 | 180 | none |  | pin 1 mark | |
| R1 | Bottom | C11702 | `passive-CHIP_0402` | 44.6000 | -21.1500 | 180 | 180 | board-4 raw reading | rotate 0, nothing applied | body | |
| R2 | Bottom | C11702 | `passive-CHIP_0402` | 44.6000 | -20.1500 | 180 | 180 | board-4 raw reading | rotate 0, nothing applied | body | |
| R3 | Top | C25741 | `passive-CHIP_0402` | 43.9500 | -70.4000 | 270 | 270 | board-4 raw reading | rotate 0, nothing applied | body | |
| R4 | Top | C25768 | `passive-CHIP_0402` | 41.4000 | -68.4000 | 270 | 270 | board-4 raw reading | rotate 0, nothing applied | body | |
| R5 | Top | C25768 | `passive-CHIP_0402` | 40.3000 | -68.4000 | 270 | 270 | board-4 raw reading | rotate 0, nothing applied | body | |
| R6 | Bottom | C25092 | `passive-CHIP_0402` | 32.2000 | -26.6000 | 0 | 0 | board-4 raw reading | rotate 0, nothing applied | body | |
| R7 | Bottom | C25741 | `passive-CHIP_0402` | 48.0000 | -65.6000 | 0 | 0 | board-4 raw reading | rotate 0, nothing applied | body | |
| R8 | Bottom | C11702 | `passive-CHIP_0402` | 51.5000 | -66.6000 | 270 | 270 | board-4 raw reading | rotate 0, nothing applied | body | |
| R9 | Bottom | C26083 | `passive-CHIP_0402` | 53.6000 | -62.4000 | 270 | 270 | board-4 raw reading | rotate 0, nothing applied | body | |
| R10 | Bottom | C25905 | `passive-CHIP_0402` | 29.6000 | -10.1000 | 180 | 180 | board-4 raw reading | rotate 0, nothing applied | body | |
| R11 | Bottom | C25905 | `passive-CHIP_0402` | 25.3500 | -10.0000 | 0 | 0 | board-4 raw reading | rotate 0, nothing applied | body | |
| R12 | Top | C25741 | `passive-CHIP_0402` | 50.7500 | -39.6500 | 90 | 90 | board-4 raw reading | rotate 0, nothing applied | body | |
| R13 | Top | C25741 | `passive-CHIP_0402` | 6.8000 | -20.8500 | 90 | 90 | board-4 raw reading | rotate 0, nothing applied | body | |
| R14 | Top | C25741 | `passive-CHIP_0402` | 6.8000 | -32.8500 | 90 | 90 | board-4 raw reading | rotate 0, nothing applied | body | |
| R15 | Top | C25741 | `passive-CHIP_0402` | 6.8000 | -44.8500 | 90 | 90 | board-4 raw reading | rotate 0, nothing applied | body | |
| R16 | Top | C25741 | `passive-CHIP_0402` | 37.1000 | -53.6000 | 270 | 270 | board-4 raw reading | rotate 0, nothing applied | body | |
| R17 | Top | C25741 | `passive-CHIP_0402` | 11.1500 | -53.6000 | 270 | 270 | board-4 raw reading | rotate 0, nothing applied | body | |
| R18 | Top | C25741 | `passive-CHIP_0402` | 27.6000 | -20.5000 | 90 | 90 | board-4 raw reading | rotate 0, nothing applied | body | |
| R19 | Top | C25741 | `passive-CHIP_0402` | 28.8500 | -20.5000 | 90 | 90 | board-4 raw reading | rotate 0, nothing applied | body | |
| R20 | Top | C11702 | `passive-CHIP_0402` | 49.5000 | -39.6500 | 270 | 270 | board-4 raw reading | rotate 0, nothing applied | body | |
| R21 | Top | C11702 | `passive-CHIP_0402` | 6.8000 | -18.9500 | 90 | 90 | board-4 raw reading | rotate 0, nothing applied | body | |
| R22 | Top | C11702 | `passive-CHIP_0402` | 6.8000 | -30.9500 | 90 | 90 | board-4 raw reading | rotate 0, nothing applied | body | |
| R23 | Top | C11702 | `passive-CHIP_0402` | 6.8000 | -42.9500 | 90 | 90 | board-4 raw reading | rotate 0, nothing applied | body | |
| R24 | Top | C11702 | `passive-CHIP_0402` | 35.8500 | -53.6000 | 90 | 90 | board-4 raw reading | rotate 0, nothing applied | body | |
| R25 | Top | C11702 | `passive-CHIP_0402` | 12.4000 | -53.6000 | 90 | 90 | board-4 raw reading | rotate 0, nothing applied | body | |
| R26 | Top | C11702 | `passive-CHIP_0402` | 26.3500 | -20.5000 | 270 | 270 | board-4 raw reading | rotate 0, nothing applied | body | |
| R27 | Top | C11702 | `passive-CHIP_0402` | 30.1000 | -20.5000 | 270 | 270 | board-4 raw reading | rotate 0, nothing applied | body | |
| R28 | Bottom | C25744 | `passive-CHIP_0402` | 29.9500 | -35.4000 | 180 | 180 | none |  | body | |
| R29 | Bottom | C25744 | `passive-CHIP_0402` | 29.9500 | -36.4500 | 180 | 180 | none |  | body | |
| R30 | Bottom | C25741 | `passive-CHIP_0402` | 45.3000 | -22.9500 | 90 | 90 | board-4 raw reading | rotate 0, nothing applied | body | |
| R31 | Bottom | C477807 | `passive-CHIP_0402` | 23.4500 | -31.6000 | 0 | 0 | none |  | body | |
| R32 | Bottom | C327334 | `passive-CHIP_0402` | 25.4000 | -31.6000 | 0 | 0 | none |  | body | |
| R34 | Bottom | C26083 | `passive-CHIP_0402` | 20.6000 | -31.6000 | 0 | 0 | board-4 raw reading | rotate 0, nothing applied | body | |
| R35 | Bottom | C11702 | `passive-CHIP_0402` | 26.5000 | -40.3500 | 0 | 0 | board-4 raw reading | rotate 0, nothing applied | body | |
| R36 | Bottom | C26083 | `passive-CHIP_0402` | 23.4500 | -44.3000 | 0 | 0 | board-4 raw reading | rotate 0, nothing applied | body | |
| R37 | Top | C137990 | `passive-CHIP_0402` | 14.2500 | -4.9000 | 90 | 90 | none |  | body | |
| R38 | Top | C25890 | `passive-CHIP_0402` | 18.4000 | -7.1000 | 0 | 0 | board-4 raw reading | rotate 0, nothing applied | body | |
| R39 | Bottom | C26083 | `passive-CHIP_0402` | 34.5000 | -30.0000 | 0 | 0 | board-4 raw reading | rotate 0, nothing applied | body | |
| R40 | Top | C17168 | `passive-CHIP_0402` | 52.2000 | -31.6500 | 90 | 90 | board-4 raw reading | rotate 0, nothing applied | body | |
| R41 | Bottom | C25741 | `passive-CHIP_0402` | 29.9500 | -34.3500 | 180 | 180 | board-4 raw reading | rotate 0, nothing applied | body | |
| R42 | Bottom | C26083 | `passive-CHIP_0402` | 18.4000 | -39.6000 | 0 | 0 | board-4 raw reading | rotate 0, nothing applied | body | |
| R43 | Bottom | C25760 | `passive-CHIP_0402` | 16.4500 | -39.6000 | 0 | 0 | none |  | body | |
| R45 | Bottom | C25741 | `passive-CHIP_0402` | 29.9500 | -37.5000 | 0 | 0 | board-4 raw reading | rotate 0, nothing applied | body | |
| R46 | Bottom | C25741 | `passive-CHIP_0402` | 32.5000 | -29.6000 | 270 | 270 | board-4 raw reading | rotate 0, nothing applied | body | |
| R47 | Top | C17168 | `passive-CHIP_0402` | 52.2000 | -29.6000 | 90 | 90 | board-4 raw reading | rotate 0, nothing applied | body | |
| R48 | Bottom | C17168 | `passive-CHIP_0402` | 33.5000 | -33.3500 | 270 | 270 | board-4 raw reading | rotate 0, nothing applied | body | |
| SW1 | Top | C115365 | `openmicrodial_rev_a-FP_ALPS_SKTDLDE010` | 3.7600 | -18.5000 | 270 | 270 | none |  | by eye (mark not certain) | |
| SW2 | Top | C115365 | `openmicrodial_rev_a-FP_ALPS_SKTDLDE010` | 3.7600 | -30.5000 | 270 | 270 | none |  | by eye (mark not certain) | |
| SW3 | Bottom | C2799946 | `switches-FP_CK_PTS636SM25` | 39.7225 | -57.0900 | 180 | 180 | none |  | by eye (two-terminal, no mark) | |
| SW4 | Bottom | C2799946 | `switches-FP_CK_PTS636SM25` | 16.2775 | -57.0900 | 180 | 180 | none |  | by eye (two-terminal, no mark) | |
| SW5 | Top | C115365 | `openmicrodial_rev_a-FP_ALPS_SKTDLDE010` | 52.2400 | -35.7430 | 90 | 90 | none |  | by eye (mark not certain) | |
| SW6 | Top | C115365 | `openmicrodial_rev_a-FP_ALPS_SKTDLDE010` | 3.7600 | -42.5000 | 270 | 270 | none |  | by eye (mark not certain) | |
| U1 | Top | C189961 | `contrib_audio_amp-SOP8P65X490X110N` | 41.5000 | -72.5000 | 270 | 180 | board-4 raw reading | rotate 270 applied | pin 1 mark | |
| U2 | Bottom | C131941 | `load_switch-FP_TI_DBV0006A` | 51.5000 | -63.5000 | 270 | 90 | board-4 raw reading | rotate 180 applied | pin 1 mark | |
| U3 | Bottom | C55247264 | `openmicrodial_rev_a-FP_DIANWEI_8_03A5_031250` | 36.6000 | -36.7500 | 180 | 180 | none |  | pin 1 mark | |
| U4 | Bottom | C97521 | `soic-SOIC8P127X790X216N` | 32.6000 | -14.2000 | binding | 90 | library binding | binding map rotate 0, offset (0, 0) mm, already in the fab row | pin 1 mark | |
| U6 | Top | C15220 | `openmicrodial_rev_a-FP_TI_RGT_VQFN16_3x3_EP1p68` | 15.0000 | -8.2000 | 0 | 0 | none |  | pin 1 mark | |
| U7 | Bottom | C147972 | `qfn-QFN10N50P250X250_1EP120X200` | 15.0000 | -42.5000 | 90 | 90 | none |  | pin 1 mark | |
| U8 | Bottom | C147972 | `qfn-QFN10N50P250X250_1EP120X200` | 22.0000 | -34.5000 | 90 | 90 | none |  | pin 1 mark | |
| U10 | Bottom | C131941 | `load_switch-FP_TI_DBV0006A` | 26.0000 | -43.0000 | 90 | 270 | board-4 raw reading | rotate 180 applied | pin 1 mark | |
| U20 | Top | C7519 | `esd-FP_SOT_23_6` | 25.5500 | -12.8000 | 270 | 180 | board-4 raw reading | rotate 270 applied | pin 1 mark | |
