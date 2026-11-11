# OpenWalkieTalkie Rev C: placement-preview checklist

For JLCPCB's assembly preview only. Stop at Component Placements. Do not save to the cart, quote or order.
**Not uploaded.** An upload needs the owner's approval for this package, by its SHA-256.

Upload `owt-revc-gerbers.zip` as the Gerber, `owt-revc-lcsc-bom.csv` as the BOM and `owt-revc-lcsc-cpl.csv` as the CPL.
Expect 4 layers, 53.43 x 92.43 mm, 41 BOM lines and 118 placements: 59 top, 59 bottom. Assemble both sides.
The run needs the **Standard** PCBA type: Economic offers one assembly side on this four-layer board. `jlc_preview.py` (conol-ai/cohdl#116) selects it.

## What this iteration tests

The preview of 2026-11-12 read 9 placements wrong with no mapping applied. This package places them with the mapping that reading implies. Each must now read right; until it does, the mapping is not confirmed. The three offsets are estimates (J1 and J3 +-0.3 mm, the switches +-0.2 mm), so read how far the leads sit from the pad centres.

| LCSC | Parts | Mapping under test | CPL row: fab, then this package | Look at |
|---|---|---|---|---|
| C177225 | J1 | rotate 180, offset (0, -0.95) mm | J1 (top) (33.0000, -84.9000) 90 -> (32.0500, -84.9000) 270 | J1: both leads on pads 1-2 (along the pads, not only at their inner end), the fitting nails on the ML/MR pads, the pin 1 mark on the pad 1 side. |
| C293630 | J3 | rotate 0, offset (0, -1.15) mm | J3 (top) (17.0000, -16.5000) 0 -> (17.0000, -15.3500) 0 | J3: the three leads on pads 1-3 with their tips on the pads, the fitting nails on the ML/MR pads; the pin 1 mark stays on the pad 1 side. |
| C115365 | SW1, SW2, SW5, SW6 | rotate 180, offset (0, -0.55) mm | SW1 (top) (3.7600, -18.5000) 270 -> (4.3100, -18.5000) 90; SW2 (top) (3.7600, -30.5000) 270 -> (4.3100, -30.5000) 90; SW5 (top) (52.2400, -35.7430) 90 -> (51.6900, -35.7430) 270; SW6 (top) (3.7600, -42.5000) 270 -> (4.3100, -42.5000) 90 | SW1 SW2 SW5 SW6: the actuator points outboard, over the board edge (SW1, SW2, SW6 left; SW5 right), not over pad MC; the terminals lie on pads 1 and 2. |
| C55247264 | U3 | rotate 180, offset (0, 0) mm | U3 (bottom) (36.6000, -36.7500) 180 -> (36.6000, -36.7500) 0 | U3: both mount ears on their pads and the 16-lead row on the 16-pad row, leads on pads on both rows; the pin 1 mark at pad 1. |
| C147972 | U7, U8 | rotate 270, offset (0, 0) mm | U7 (bottom) (15.0000, -42.5000) 90 -> (15.0000, -42.5000) 0; U8 (bottom) (22.0000, -34.5000) 90 -> (22.0000, -34.5000) 0 | U7 U8: the pin 1 mark at pad 1 (bottom side), the terminal rows along our pad rows. |

An offset is in the footprint's own frame (+y down, as KiCad draws the footprint): where the assembler's origin has to sit relative to the footprint origin. On the board it is turned by the placement's rotation; on the bottom its x is mirrored first.
Every other placement was confirmed on 2026-11-12 and is placed as it was then; it should read as it did.

## Left out, so not previewed

A part that is not in the BOM and CPL is not drawn. Its orientation stays unmeasured after this preview.

| Designator | LCSC | Why |
|---|---|---|
| U5 | C47355842 | SF32LB525UC6: LCSC stock 0 when read on 2026-11-11. The preview tool stops at the BOM step on any stock warning. |
| Y1 | none | E1SB48E001G00E 48 MHz crystal: no LCSC number, so the BOM step cannot match it. |
| AE1 | C2917717 | 2450AT18B100E antenna: stock shortage at the BOM step of the first preview run on 2026-11-12. |
| R32 | C327334 | RC0402FR-07604KL: stock shortage at the BOM step of the first preview run on 2026-11-12. |
| SW3 | C2799946 | Stock shortage at the BOM step of the first preview run on 2026-11-12. |
| SW4 | C2799946 | Stock shortage at the BOM step of the first preview run on 2026-11-12. |
| MK1 | C2688664 | MP34DT05TR-A: the BOM step matched no part for this number in the first preview run on 2026-11-12. |

DNP sites (C52, C53, C58, L6, R44), test points and mounting holes are not assembled and are absent, as in the fab package.

## Stock risks, kept in

The preview tool stops at the BOM step on any stock warning. If one of these is short, move it to `left-out.csv` and rebuild.

| LCSC | Designators | Stock | Read on | Note |
|---|---|---|---|---|
| C147972 | U7, U8 | 50 | 2026-11-11 | RT6150BGQW; two per board. |
| C115365 | SW1, SW2, SW5, SW6 | 155 | 2026-11-11 | SKTDLDE010; four per board. |
| C15220 | U6 | 687 | 2026-11-11 | BQ24073RGTR. |

## Corrections applied

Rows not listed here are at the convention value (top = KiCad angle, bottom = 180 - KiCad angle) with no evidence.

| LCSC | Board footprint | Rotate | Offset mm | Evidence | State | Detail |
|---|---|---|---|---|---|---|
| C15849 | `passive-CHIP_0603` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | C1 C3 C4 C5 C11 C21 C49: 7 bodies: |axis| <= 0.2 deg mod 180, centre <= 0.023 mm (limit 0.206). |
| C107039 | `passive-CHIP_0603` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | C2: 1 bodies: |axis| <= 0.1 deg mod 180, centre <= 0.013 mm (limit 0.206). |
| C113780 | `passive-CHIP_0402` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | C6 C32 C33 C34 C35 C36 C37 C38 C39 C50: 10 bodies: |axis| <= 0.4 deg mod 180, centre <= 0.024 mm (limit 0.127). |
| C97946 | `passive-CHIP_1206` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | C7: 1 bodies: |axis| <= 0.0 deg mod 180, centre <= 0.021 mm (limit 0.250). |
| C107006 | `passive-CHIP_0402` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | C8 C9 C10: 3 bodies: |axis| <= 0.3 deg mod 180, centre <= 0.022 mm (limit 0.128). |
| C15850 | `passive-CHIP_0805` | 0 | (0, 0) | library binding | confirmed | Frame record c2b1-c0805-c15850. Confirmed on this board on 2026-11-12 (C12). |
| C23733 | `passive-CHIP_0402` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | C13 C17 C28 C29 C30: 5 bodies: |axis| <= 0.4 deg mod 180, centre <= 0.014 mm (limit 0.127). |
| C107369 | `passive-CHIP_0402` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | C14 C16 C23 C24 C27 C31: 6 bodies: |axis| <= 0.7 deg mod 180, centre <= 0.018 mm (limit 0.127). |
| C52923 | `passive-CHIP_0402` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | C15 C26: 2 bodies: |axis| <= 0.7 deg mod 180, centre <= 0.014 mm (limit 0.127). |
| C19666 | `passive-CHIP_0603` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | C18 C40 C41 C48: 4 bodies: |axis| <= 0.2 deg mod 180, centre <= 0.014 mm (limit 0.206). |
| C1525 | `passive-CHIP_0402` | 0 | (0, 0) | library binding | confirmed | Frame record c2b1-c0402-c1525. Confirmed on this board on 2026-11-12 (C19 C20 C22). |
| C106255 | `passive-CHIP_0402` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | C25: 1 bodies: |axis| <= 0.0 deg mod 180, centre <= 0.014 mm (limit 0.127). |
| C342827 | `passive-CHIP_1206` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | C42 C43 C44 C45 C46 C47: 6 bodies: |axis| <= 0.1 deg mod 180, centre <= 0.018 mm (limit 0.250). |
| C1548 | `passive-CHIP_0402` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | C57: 1 bodies: |axis| <= 0.1 deg mod 180, centre <= 0.009 mm (limit 0.127). |
| C177225 | `openmicrodial_rev_a-FP_MOLEX_53261_0271` | 180 | (0, -0.95) | Rev C preview 2026-11-12 | under test | J1: model a half turn from the land (leads away from pads 1-2, pin 1 mark opposite pad 1); turned, its lead tips only reach the pads' inner end. Offset estimated from the model's fitting-nail strips against the ML/MR pads, +-0.3 mm. CPL row becomes (32.0500, -84.9000) rotation 270. |
| C596732 | `openmicrodial_rev_a-FP_HIROSE_FH34SRJ_4S` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | J2: by eye, the model's mount tabs sit on the MP pads, centred within 0.15 mm; the model has no pin 1 mark and its contact row is not seen. |
| C293630 | `openmicrodial_rev_b-FP_MOLEX_53261_0371` | 0 | (0, -1.15) | Rev C preview 2026-11-12 | under test | J3: rotation right (pin 1 mark on the pin 1 side), but lead tips end 0.11 mm short of pads 1-3. Offset estimated from the fitting-nail strips against the ML/MR pads, +-0.3 mm. CPL row becomes (17.0000, -15.3500) rotation 0. |
| C165948 | `usb-connectors-type_c-FP_USB_C_Receptacle_HRO_TYPE_C_31_M_12` | 0 | (0, -1.55) | library binding | confirmed | Frame record usbc-origin-c165948. Confirmed on this board on 2026-11-12 (J5). |
| C16902 | `openmicrodial_rev_a-FP_MURATA_BLM31PG_1P5A_REFLOW` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | L1 L2: 2 bodies: |axis| <= 0.0 deg mod 180, centre <= 0.023 mm (limit 0.250). |
| C97018 | `passive-FP_L_WPN201610U_2016Metric` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | L3: 1 bodies: |axis| <= 0.0 deg mod 180, centre <= 0.012 mm (limit 0.175). |
| C337893 | `passive-FP_L_0806_2016Metric` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | L4 L5: 2 bodies: |axis| <= 0.0 deg mod 180, centre <= 0.013 mm (limit 0.237). |
| C11702 | `passive-CHIP_0402` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | R1 R2 R8 R20 R21 R22 R23 R24 R25 R26 R27 R35: 12 bodies: |axis| <= 0.4 deg mod 180, centre <= 0.028 mm (limit 0.127). |
| C25741 | `passive-CHIP_0402` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | R3 R7 R12 R13 R14 R15 R16 R17 R18 R19 R30 R41 R45 R46: 14 bodies: |axis| <= 0.3 deg mod 180, centre <= 0.020 mm (limit 0.127). |
| C25768 | `passive-CHIP_0402` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | R4 R5: 2 bodies: |axis| <= 0.2 deg mod 180, centre <= 0.020 mm (limit 0.127). |
| C25092 | `passive-CHIP_0402` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | R6: 1 bodies: |axis| <= 0.1 deg mod 180, centre <= 0.014 mm (limit 0.127). |
| C26083 | `passive-CHIP_0402` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | R9 R34 R36 R39 R42: 5 bodies: |axis| <= 0.1 deg mod 180, centre <= 0.020 mm (limit 0.127). |
| C25905 | `passive-CHIP_0402` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | R10 R11: 2 bodies: |axis| <= 0.4 deg mod 180, centre <= 0.008 mm (limit 0.127). |
| C25744 | `passive-CHIP_0402` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | R28 R29: 2 bodies: |axis| <= 0.1 deg mod 180, centre <= 0.012 mm (limit 0.127). |
| C477807 | `passive-CHIP_0402` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | R31: 1 bodies: |axis| <= 0.1 deg mod 180, centre <= 0.003 mm (limit 0.127). |
| C137990 | `passive-CHIP_0402` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | R37: 1 bodies: |axis| <= 0.0 deg mod 180, centre <= 0.013 mm (limit 0.128). |
| C25890 | `passive-CHIP_0402` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | R38: 1 bodies: |axis| <= 0.4 deg mod 180, centre <= 0.014 mm (limit 0.127). |
| C17168 | `passive-CHIP_0402` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | R40 R47 R48: 3 bodies: |axis| <= 0.5 deg mod 180, centre <= 0.025 mm (limit 0.127). |
| C25760 | `passive-CHIP_0402` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | R43: 1 bodies: |axis| <= 0.1 deg mod 180, centre <= 0.014 mm (limit 0.128). |
| C115365 | `openmicrodial_rev_a-FP_ALPS_SKTDLDE010` | 180 | (0, -0.55) | Rev C preview 2026-11-12 | under test | SW1 SW2 SW5 SW6: model's actuator points inboard over pad MC; its terminals lie on the ML/MR side. Read by eye (no pin 1 mark). Offset is the land's pad-centre offset, estimated, +-0.2 mm. CPL rows become SW1 (4.3100, -18.5000) 90; SW2 (4.3100, -30.5000) 90; SW6 (4.3100, -42.5000) 90; SW5 (51.6900, -35.7430) 270. |
| C189961 | `contrib_audio_amp-SOP8P65X490X110N` | 270 | (0, 0) | Rev C preview 2026-11-12 | confirmed | U1: mark at (+0.94, -2.87) from origin, pad 1 at (+0.97, -2.17); delta 5.9 deg. |
| C131941 | `load_switch-FP_TI_DBV0006A` | 180 | (0, 0) | Rev C preview 2026-11-12 | confirmed | U2 U10: delta 357.5 deg; delta 358.9 deg. |
| C55247264 | `openmicrodial_rev_a-FP_DIANWEI_8_03A5_031250` | 180 | (0, 0) | Rev C preview 2026-11-12 | under test | U3: pin 1 mark 184.8 deg from pad 1; mount ears and the 16-lead row on the wrong side. CPL rotation becomes 0. No offset evident; check the leads on both pad rows next time. |
| C97521 | `soic-SOIC8P127X790X216N` | 0 | (0, 0) | library binding | confirmed | Frame record c2b1-soic8_208-c97521. Confirmed on this board on 2026-11-12 (U4). |
| C15220 | `openmicrodial_rev_a-FP_TI_RGT_VQFN16_3x3_EP1p68` | 0 | (0, 0) | Rev C preview 2026-11-12 | confirmed | U6: mark at (-1.93, -0.81) from origin, pad 1 at (-1.46, -0.75); delta 4.2 deg. |
| C147972 | `qfn-QFN10N50P250X250_1EP120X200` | 270 | (0, 0) | Rev C preview 2026-11-12 | under test | U7 U8: pin 1 mark 261.7 deg counter-clockwise of pad 1 seen from the top, bottom side, on both. CPL rotation becomes 0. |
| C7519 | `esd-FP_SOT_23_6` | 270 | (0, 0) | Rev C preview 2026-11-12 | confirmed | U20: mark at (+0.90, -1.69) from origin, pad 1 at (+0.95, -1.15); delta 11.7 deg. |

## What the tool checks, and what needs reading by eye

- `body`: a chip part; the tool reads its drawn body along its pads (mod 180) and centred.
- `pin 1 mark`: the tool needs one pin 1 mark on the part; `measure_preview.py measure` gives its angle against pad 1.
- `by eye`: the tool has no evidence type for it. Expect "manual review"; read it from a crop of the export.

- body: 96: C1, C2, C3, C4, C5, C6, C7, C8, C9, C10, C11, C12, C13, C14, C15, C16, C17, C18, C19, C20, C21, C22, C23, C24, C25, C26, C27, C28, C29, C30, C31, C32, C33, C34, C35, C36, C37, C38, C39, C40, C41, C42, C43, C44, C45, C46, C47, C48, C49, C50, C57, R1, R2, R3, R4, R5, R6, R7, R8, R9, R10, R11, R12, R13, R14, R15, R16, R17, R18, R19, R20, R21, R22, R23, R24, R25, R26, R27, R28, R29, R30, R31, R34, R35, R36, R37, R38, R39, R40, R41, R42, R43, R45, R46, R47, R48
- by eye (mark not certain): 8: J1, J2, J3, J5, SW1, SW2, SW5, SW6
- by eye (two-terminal, no mark): 5: L1, L2, L3, L4, L5
- pin 1 mark: 9: U1, U2, U3, U4, U6, U7, U8, U10, U20

On 2026-11-12 the tool sent U1, U2, U7 and U8 to "manual review" although each has a pin 1 mark (a chip part was nearer the mark), and passed J1 and J3 on the mark alone. `measure_preview.py measure` reads the marks whatever the tool's status says; read the leads of J1, J3 and U3 and the switches from crops.
U4, J5, U1, U2, U10 and U20 were confirmed on 2026-11-12 with the mapping they have here: U4's pin 1 toward the lower left seen from the top, J5's shell pegs in their holes.

## Placements

"Fab rotation" is the fab CPL's, the convention value unless the row is a library binding. Mid X, Mid Y and "Preview rotation" are this package's.

| Ref | Side | LCSC | Board footprint | Mid X | Mid Y | Fab rotation | Preview rotation | Evidence | State | Applied | Check | Reading |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| C1 | Top | C15849 | `passive-CHIP_0603` | 43.5000 | -68.6000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C2 | Bottom | C107039 | `passive-CHIP_0603` | 46.6000 | -20.6500 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C3 | Top | C15849 | `passive-CHIP_0603` | 41.6500 | -65.8500 | 270 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C4 | Top | C15849 | `passive-CHIP_0603` | 40.0500 | -65.8500 | 270 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C5 | Bottom | C15849 | `passive-CHIP_0603` | 51.0000 | -60.6000 | 180 | 180 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C6 | Bottom | C113780 | `passive-CHIP_0402` | 53.6000 | -64.9000 | 270 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C7 | Bottom | C97946 | `passive-CHIP_1206` | 48.0000 | -76.3000 | 180 | 180 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C8 | Top | C107006 | `passive-CHIP_0402` | 45.4500 | -81.2000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C9 | Top | C107006 | `passive-CHIP_0402` | 37.5500 | -81.2000 | 180 | 180 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C10 | Top | C107006 | `passive-CHIP_0402` | 41.5000 | -81.2000 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C11 | Top | C15849 | `passive-CHIP_0603` | 41.5000 | -76.4000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C12 | Bottom | C15850 | `passive-CHIP_0805` | 41.5000 | -79.4000 | binding | 180 | library binding | confirmed | binding map rotate 0, offset (0, 0) mm, already in the fab row | body | |
| C13 | Bottom | C23733 | `passive-CHIP_0402` | 34.5000 | -28.9000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C14 | Bottom | C107369 | `passive-CHIP_0402` | 43.8500 | -22.9500 | 180 | 180 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C15 | Top | C52923 | `passive-CHIP_0402` | 43.9000 | -23.1500 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C16 | Bottom | C107369 | `passive-CHIP_0402` | 44.6000 | -28.0500 | 180 | 180 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C17 | Bottom | C23733 | `passive-CHIP_0402` | 34.1500 | -20.4000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C18 | Bottom | C19666 | `passive-CHIP_0603` | 36.7500 | -33.5500 | 180 | 180 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C19 | Bottom | C1525 | `passive-CHIP_0402` | 34.7500 | -33.3500 | binding | 90 | library binding | confirmed | binding map rotate 0, offset (0, 0) mm, already in the fab row | body | |
| C20 | Bottom | C1525 | `passive-CHIP_0402` | 32.4500 | -33.3500 | binding | 90 | library binding | confirmed | binding map rotate 0, offset (0, 0) mm, already in the fab row | body | |
| C21 | Bottom | C15849 | `passive-CHIP_0603` | 29.4000 | -38.7000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C22 | Bottom | C1525 | `passive-CHIP_0402` | 36.4500 | -18.2000 | binding | 270 | library binding | confirmed | binding map rotate 0, offset (0, 0) mm, already in the fab row | body | |
| C23 | Top | C107369 | `passive-CHIP_0402` | 37.2000 | -17.5500 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C24 | Bottom | C107369 | `passive-CHIP_0402` | 32.2000 | -22.6000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C25 | Bottom | C106255 | `passive-CHIP_0402` | 32.2000 | -23.6000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C26 | Bottom | C52923 | `passive-CHIP_0402` | 32.2000 | -24.6000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C27 | Top | C107369 | `passive-CHIP_0402` | 38.3000 | -17.5500 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C28 | Top | C23733 | `passive-CHIP_0402` | 39.4000 | -17.5500 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C29 | Bottom | C23733 | `passive-CHIP_0402` | 34.1500 | -21.4000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C30 | Bottom | C23733 | `passive-CHIP_0402` | 34.1500 | -22.4000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C31 | Top | C107369 | `passive-CHIP_0402` | 36.1000 | -17.5500 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C32 | Top | C113780 | `passive-CHIP_0402` | 49.5000 | -41.7500 | 270 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C33 | Top | C113780 | `passive-CHIP_0402` | 6.8000 | -17.0500 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C34 | Top | C113780 | `passive-CHIP_0402` | 6.8000 | -29.0500 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C35 | Top | C113780 | `passive-CHIP_0402` | 6.8000 | -41.0500 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C36 | Top | C113780 | `passive-CHIP_0402` | 34.6000 | -53.6000 | 270 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C37 | Top | C113780 | `passive-CHIP_0402` | 13.6500 | -53.6000 | 270 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C38 | Top | C113780 | `passive-CHIP_0402` | 26.8500 | -22.2500 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C39 | Top | C113780 | `passive-CHIP_0402` | 29.6000 | -22.2500 | 180 | 180 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C40 | Top | C19666 | `passive-CHIP_0603` | 11.1000 | -8.5500 | 180 | 180 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C41 | Top | C19666 | `passive-CHIP_0603` | 16.6000 | -5.1000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C42 | Bottom | C342827 | `passive-CHIP_1206` | 12.0500 | -43.5000 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C43 | Bottom | C342827 | `passive-CHIP_1206` | 17.9500 | -43.5000 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C44 | Bottom | C342827 | `passive-CHIP_1206` | 20.4000 | -43.5000 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C45 | Bottom | C342827 | `passive-CHIP_1206` | 19.0500 | -35.5000 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C46 | Bottom | C342827 | `passive-CHIP_1206` | 24.9500 | -35.5000 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C47 | Bottom | C342827 | `passive-CHIP_1206` | 27.4000 | -35.5000 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C48 | Top | C19666 | `passive-CHIP_0603` | 18.5000 | -9.3500 | 270 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C49 | Bottom | C15849 | `passive-CHIP_0603` | 26.5000 | -46.0000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C50 | Bottom | C113780 | `passive-CHIP_0402` | 24.4500 | -40.3500 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| C57 | Bottom | C1548 | `passive-CHIP_0402` | 43.4000 | -29.5500 | 270 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| J1 | Top | C177225 | `openmicrodial_rev_a-FP_MOLEX_53261_0271` | 32.0500 | -84.9000 | 90 | 270 | Rev C preview 2026-11-12 | under test | rotate 180, offset (0, -0.95) mm applied: fab row (33.0000, -84.9000) 90 becomes (32.0500, -84.9000) 270 | by eye (mark not certain) | |
| J2 | Top | C596732 | `openmicrodial_rev_a-FP_HIROSE_FH34SRJ_4S` | 32.5000 | -16.8000 | 270 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | by eye (mark not certain) | |
| J3 | Top | C293630 | `openmicrodial_rev_b-FP_MOLEX_53261_0371` | 17.0000 | -15.3500 | 0 | 0 | Rev C preview 2026-11-12 | under test | rotate 0, offset (0, -1.15) mm applied: fab row (17.0000, -16.5000) 0 becomes (17.0000, -15.3500) 0 | by eye (mark not certain) | |
| J5 | Top | C165948 | `usb-connectors-type_c-FP_USB_C_Receptacle_HRO_TYPE_C_31_M_12` | 28.0000 | -5.5500 | binding | 180 | library binding | confirmed | binding map rotate 0, offset (0, -1.55) mm, already in the fab row | by eye (mark not certain) | |
| L1 | Top | C16902 | `openmicrodial_rev_a-FP_MURATA_BLM31PG_1P5A_REFLOW` | 43.3000 | -79.6500 | 270 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | by eye (two-terminal, no mark) | |
| L2 | Top | C16902 | `openmicrodial_rev_a-FP_MURATA_BLM31PG_1P5A_REFLOW` | 39.7000 | -79.6500 | 270 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | by eye (two-terminal, no mark) | |
| L3 | Bottom | C97018 | `passive-FP_L_WPN201610U_2016Metric` | 34.0000 | -18.7000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | by eye (two-terminal, no mark) | |
| L4 | Bottom | C337893 | `passive-FP_L_0806_2016Metric` | 15.0000 | -45.5000 | 180 | 180 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | by eye (two-terminal, no mark) | |
| L5 | Bottom | C337893 | `passive-FP_L_0806_2016Metric` | 22.0000 | -37.5000 | 180 | 180 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | by eye (two-terminal, no mark) | |
| R1 | Bottom | C11702 | `passive-CHIP_0402` | 44.6000 | -21.1500 | 180 | 180 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R2 | Bottom | C11702 | `passive-CHIP_0402` | 44.6000 | -20.1500 | 180 | 180 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R3 | Top | C25741 | `passive-CHIP_0402` | 43.9500 | -70.4000 | 270 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R4 | Top | C25768 | `passive-CHIP_0402` | 41.4000 | -68.4000 | 270 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R5 | Top | C25768 | `passive-CHIP_0402` | 40.3000 | -68.4000 | 270 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R6 | Bottom | C25092 | `passive-CHIP_0402` | 32.2000 | -26.6000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R7 | Bottom | C25741 | `passive-CHIP_0402` | 48.0000 | -65.6000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R8 | Bottom | C11702 | `passive-CHIP_0402` | 51.5000 | -66.6000 | 270 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R9 | Bottom | C26083 | `passive-CHIP_0402` | 53.6000 | -62.4000 | 270 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R10 | Bottom | C25905 | `passive-CHIP_0402` | 29.6000 | -10.1000 | 180 | 180 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R11 | Bottom | C25905 | `passive-CHIP_0402` | 25.3500 | -10.0000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R12 | Top | C25741 | `passive-CHIP_0402` | 50.7500 | -39.6500 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R13 | Top | C25741 | `passive-CHIP_0402` | 6.8000 | -20.8500 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R14 | Top | C25741 | `passive-CHIP_0402` | 6.8000 | -32.8500 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R15 | Top | C25741 | `passive-CHIP_0402` | 6.8000 | -44.8500 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R16 | Top | C25741 | `passive-CHIP_0402` | 37.1000 | -53.6000 | 270 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R17 | Top | C25741 | `passive-CHIP_0402` | 11.1500 | -53.6000 | 270 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R18 | Top | C25741 | `passive-CHIP_0402` | 27.6000 | -20.5000 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R19 | Top | C25741 | `passive-CHIP_0402` | 28.8500 | -20.5000 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R20 | Top | C11702 | `passive-CHIP_0402` | 49.5000 | -39.6500 | 270 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R21 | Top | C11702 | `passive-CHIP_0402` | 6.8000 | -18.9500 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R22 | Top | C11702 | `passive-CHIP_0402` | 6.8000 | -30.9500 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R23 | Top | C11702 | `passive-CHIP_0402` | 6.8000 | -42.9500 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R24 | Top | C11702 | `passive-CHIP_0402` | 35.8500 | -53.6000 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R25 | Top | C11702 | `passive-CHIP_0402` | 12.4000 | -53.6000 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R26 | Top | C11702 | `passive-CHIP_0402` | 26.3500 | -20.5000 | 270 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R27 | Top | C11702 | `passive-CHIP_0402` | 30.1000 | -20.5000 | 270 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R28 | Bottom | C25744 | `passive-CHIP_0402` | 29.9500 | -35.4000 | 180 | 180 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R29 | Bottom | C25744 | `passive-CHIP_0402` | 29.9500 | -36.4500 | 180 | 180 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R30 | Bottom | C25741 | `passive-CHIP_0402` | 45.3000 | -22.9500 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R31 | Bottom | C477807 | `passive-CHIP_0402` | 23.4500 | -31.6000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R34 | Bottom | C26083 | `passive-CHIP_0402` | 20.6000 | -31.6000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R35 | Bottom | C11702 | `passive-CHIP_0402` | 26.5000 | -40.3500 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R36 | Bottom | C26083 | `passive-CHIP_0402` | 23.4500 | -44.3000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R37 | Top | C137990 | `passive-CHIP_0402` | 14.2500 | -4.9000 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R38 | Top | C25890 | `passive-CHIP_0402` | 18.4000 | -7.1000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R39 | Bottom | C26083 | `passive-CHIP_0402` | 34.5000 | -30.0000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R40 | Top | C17168 | `passive-CHIP_0402` | 52.2000 | -31.6500 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R41 | Bottom | C25741 | `passive-CHIP_0402` | 29.9500 | -34.3500 | 180 | 180 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R42 | Bottom | C26083 | `passive-CHIP_0402` | 18.4000 | -39.6000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R43 | Bottom | C25760 | `passive-CHIP_0402` | 16.4500 | -39.6000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R45 | Bottom | C25741 | `passive-CHIP_0402` | 29.9500 | -37.5000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R46 | Bottom | C25741 | `passive-CHIP_0402` | 32.5000 | -29.6000 | 270 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R47 | Top | C17168 | `passive-CHIP_0402` | 52.2000 | -29.6000 | 90 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| R48 | Bottom | C17168 | `passive-CHIP_0402` | 33.5000 | -33.3500 | 270 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | body | |
| SW1 | Top | C115365 | `openmicrodial_rev_a-FP_ALPS_SKTDLDE010` | 4.3100 | -18.5000 | 270 | 90 | Rev C preview 2026-11-12 | under test | rotate 180, offset (0, -0.55) mm applied: fab row (3.7600, -18.5000) 270 becomes (4.3100, -18.5000) 90 | by eye (mark not certain) | |
| SW2 | Top | C115365 | `openmicrodial_rev_a-FP_ALPS_SKTDLDE010` | 4.3100 | -30.5000 | 270 | 90 | Rev C preview 2026-11-12 | under test | rotate 180, offset (0, -0.55) mm applied: fab row (3.7600, -30.5000) 270 becomes (4.3100, -30.5000) 90 | by eye (mark not certain) | |
| SW5 | Top | C115365 | `openmicrodial_rev_a-FP_ALPS_SKTDLDE010` | 51.6900 | -35.7430 | 90 | 270 | Rev C preview 2026-11-12 | under test | rotate 180, offset (0, -0.55) mm applied: fab row (52.2400, -35.7430) 90 becomes (51.6900, -35.7430) 270 | by eye (mark not certain) | |
| SW6 | Top | C115365 | `openmicrodial_rev_a-FP_ALPS_SKTDLDE010` | 4.3100 | -42.5000 | 270 | 90 | Rev C preview 2026-11-12 | under test | rotate 180, offset (0, -0.55) mm applied: fab row (3.7600, -42.5000) 270 becomes (4.3100, -42.5000) 90 | by eye (mark not certain) | |
| U1 | Top | C189961 | `contrib_audio_amp-SOP8P65X490X110N` | 41.5000 | -72.5000 | 270 | 180 | Rev C preview 2026-11-12 | confirmed | rotate 270, offset (0, 0) mm applied: fab row (41.5000, -72.5000) 270 becomes (41.5000, -72.5000) 180 | pin 1 mark | |
| U2 | Bottom | C131941 | `load_switch-FP_TI_DBV0006A` | 51.5000 | -63.5000 | 270 | 90 | Rev C preview 2026-11-12 | confirmed | rotate 180, offset (0, 0) mm applied: fab row (51.5000, -63.5000) 270 becomes (51.5000, -63.5000) 90 | pin 1 mark | |
| U3 | Bottom | C55247264 | `openmicrodial_rev_a-FP_DIANWEI_8_03A5_031250` | 36.6000 | -36.7500 | 180 | 0 | Rev C preview 2026-11-12 | under test | rotate 180, offset (0, 0) mm applied: fab row (36.6000, -36.7500) 180 becomes (36.6000, -36.7500) 0 | pin 1 mark | |
| U4 | Bottom | C97521 | `soic-SOIC8P127X790X216N` | 32.6000 | -14.2000 | binding | 90 | library binding | confirmed | binding map rotate 0, offset (0, 0) mm, already in the fab row | pin 1 mark | |
| U6 | Top | C15220 | `openmicrodial_rev_a-FP_TI_RGT_VQFN16_3x3_EP1p68` | 15.0000 | -8.2000 | 0 | 0 | Rev C preview 2026-11-12 | confirmed | rotate 0, no offset: nothing applied | pin 1 mark | |
| U7 | Bottom | C147972 | `qfn-QFN10N50P250X250_1EP120X200` | 15.0000 | -42.5000 | 90 | 0 | Rev C preview 2026-11-12 | under test | rotate 270, offset (0, 0) mm applied: fab row (15.0000, -42.5000) 90 becomes (15.0000, -42.5000) 0 | pin 1 mark | |
| U8 | Bottom | C147972 | `qfn-QFN10N50P250X250_1EP120X200` | 22.0000 | -34.5000 | 90 | 0 | Rev C preview 2026-11-12 | under test | rotate 270, offset (0, 0) mm applied: fab row (22.0000, -34.5000) 90 becomes (22.0000, -34.5000) 0 | pin 1 mark | |
| U10 | Bottom | C131941 | `load_switch-FP_TI_DBV0006A` | 26.0000 | -43.0000 | 90 | 270 | Rev C preview 2026-11-12 | confirmed | rotate 180, offset (0, 0) mm applied: fab row (26.0000, -43.0000) 90 becomes (26.0000, -43.0000) 270 | pin 1 mark | |
| U20 | Top | C7519 | `esd-FP_SOT_23_6` | 25.5500 | -12.8000 | 270 | 180 | Rev C preview 2026-11-12 | confirmed | rotate 270, offset (0, 0) mm applied: fab row (25.5500, -12.8000) 270 becomes (25.5500, -12.8000) 180 | pin 1 mark | |
