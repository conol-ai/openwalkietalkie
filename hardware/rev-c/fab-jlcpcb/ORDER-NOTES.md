# JLCPCB order package — OpenWalkieTalkie Rev C main board (BodyV6.13)

Prepared 2026-11-11 from `../main/main-revc.kicad_pcb`. The board is Rev B's
copper: the 13 Gerber, paste and drill files in the zip equal Rev B's apart
from header metadata
([`../verification/fab-compare-vs-rev-b.json`](../verification/fab-compare-vs-rev-b.json)).
KiCad 10.0.7 DRC with zones refilled, all severities: 0 violations,
0 unconnected.

**Do not order from this placement file as it stands.** A preview package
derived from it went through JLCPCB's placement preview on 2026-11-12
([record](../preview/2026-11-12/README.md)); nothing was ordered. That
preview shows 13 rows of `main-cpl-jlcpcb.csv` to be wrong and left seven
parts unread. See "What the 2026-11-12 preview found" below, and read "What
the BOM and CPL rest on" before using the placement file. The files in this
directory have not been changed since.

This package covers the main board only. The encoder board did not change:
order r6's `encoder/` (or `encoder-wheel/`) package from
`../../rev-a/pcb-release-bodyv6.13-20260923-r6/fab-jlcpcb/`.

| Upload this | File |
|---|---|
| Gerber + drill (PCB order) | `main-revc-jlcpcb-gerber.zip` |
| BOM (PCBA) | `main-bom-jlcpcb.csv` |
| CPL / pick-and-place (PCBA) | `main-cpl-jlcpcb.csv` |
| Placement check list | `main-orientation-check.csv` |
| Where each row comes from (not for upload) | `main-placement-sources.csv` |

The zip holds 13 files:
- 4 copper layers, 2 mask, 2 silkscreen, 2 paste (for the stencil) and the
  outline
- PTH and NPTH Excellon drill files

The plots themselves are not committed a second time; Rev B's are in
`../../rev-b/main/gerber`, `stencil` and `drill`.

## What the BOM and CPL rest on

Rev B's BOM and CPL were written from the board by formula, with LCSC
numbers from a hand-kept table. The maintainer has since said that the
Rev B boards used the repository's Gerbers, but that the BOM and placement
file were changed by hand in JLC's interface at order time, for stock and
for coordinates. So Rev B's committed BOM and CPL are not what was placed,
and the corrections were not recorded.

In Rev C each row has one stated source, listed per designator in
`main-placement-sources.csv` and per line in the BOM's last column:

- **Library binding (6 placements: C12, C19, C20, C22, J5, U4).** The LCSC
  number, the origin and the rotation come from
  `cohdl build --supplier lcsc --bottom-rotation lcsc=mirrored`. Each binding
  cites a frame-evidence record in the CoHDL library (a JLC placement
  preview read on 2026-10-30); the ids are in
  [`../source/main/frame-evidence.json`](../source/main/frame-evidence.json).
  All six numbers are the ones Rev B ordered.
- **Fallback table (118 placements).** The LCSC number comes from
  [`../source/main/supplier-fallback.csv`](../source/main/supplier-fallback.csv),
  copied per designator from Rev B's order BOM. The position is the
  footprint origin and the rotation is the convention angle, as in Rev B.
- **No LCSC part (Y1).** As in Rev B; see below.

Three things follow.

**(a) U4 and J5 differ from Rev B's committed CPL.**

| Part | Rev B CPL | Rev C CPL | Why |
|---|---|---|---|
| U4 W25Q128JVSIQ (C97521), bottom | (32.6000, −14.2000), rotation **180** | (32.6000, −14.2000), rotation **90** | Frame record `c2b1-soic8_208-c97521`: JLC's model for this part sits a quarter turn from the land as Rev B drew it. `soic` 0.3.0 turned the land to match, so the binding's map is zero and the CPL angle follows the land. |
| J5 TYPE-C-31-M-12 (C165948), top | (28.0000, **−4.0000**), rotation 180 | (28.0000, **−5.5500**), rotation 180 | Frame record `usbc-origin-c165948`: JLC's model origin is 1.55 mm from the footprint origin along the mating axis. Rev B's notes already flagged a 1.48 mm origin offset for J5 to be checked in the preview. |

These are the kind of correction that was made by hand at order time, but
**they have not been confirmed against the actual order.** The library's
readings were taken on other boards. Check both parts in the preview: U4's
pin 1 must land toward the lower left seen from the top (pad at
(29.012, −16.105)), and J5's shell pegs must sit in their holes.

**(b) The other 123 rows are uncorrected.** They are the same values as in
Rev B's committed CPL: footprint origin, top rotation = KiCad angle, bottom
rotation = 180 − KiCad angle. For the four bound capacitors that is also
what the library says. For the 119 unbound parts nothing has checked the
angle or origin against JLC's part model, exactly as in Rev B. They still
need the preview check below.

**(c) The as-ordered files are requested.** The maintainer has been asked
for the BOM and CPL as exported from JLC's order history. Those would
replace the guesswork in (a) and (b). Until they arrive, treat this package
as Rev B's with two rows improved, not as a record of a known-good order.

## What the 2026-11-12 preview found

The preview used this package's Gerbers, and its BOM and CPL without seven
parts and with three candidate rotations applied
([`../preview/README.md`](../preview/README.md)).

**PCBA type: Standard.** On this four-layer board JLCPCB's Economic PCBA
type offers one assembly side; Both Sides is disabled. The board has parts
on both sides, so the order needs Standard.

**Parts JLCPCB cannot assemble as the BOM stands** (read at the BOM step on
2026-11-11 and 2026-11-12; stock changes):

| Part | LCSC | Why |
|---|---|---|
| U5 SF32LB525UC6 (the MCU) | C47355842 | Stock 0 on 2026-11-11. |
| Y1 E1SB48E001G00E | none | No LCSC number; see the decision table below. |
| AE1 2450AT18B100E | C2917717 | Stock shortage on 2026-11-12. |
| R32 604k RC0402FR-07604KL | C327334 | Stock shortage on 2026-11-12. |
| SW3, SW4 PTS636SM25SMTRLFS | C2799946 | Stock shortage on 2026-11-12. |
| MK1 MP34DT05TR-A | C2688664 | The BOM step matched no part to this number. |

Each needs stock, a substitute, global sourcing or consignment before an
order, and none of the seven has had its placement read: a part that cannot
pass the BOM step is not drawn in the preview.

The other 41 part numbers were detected and confirmed with no stock warning.
C147972 (U7, U8; 50 in stock on 2026-11-11), C115365 (the four side
switches; 155) and C15220 (U6; 687) were low.

**CPL rows that are wrong as committed.** Rotations are JLC rotations;
positions are CPL coordinates.

| Part | Row here | Should be | State |
|---|---|---|---|
| U1 NS4150B | rotation 270 | 180 | confirmed in the preview |
| U2 TPS22918DBVR | 270 | 90 | confirmed |
| U10 TPS22918DBVR | 90 | 270 | confirmed |
| U20 USBLC6-2SC6 | 270 | 180 | confirmed |
| U7, U8 RT6150BGQW | 90 | 0 | read; not yet previewed with the change |
| U3 display connector | 180 | 0 | read; not yet previewed with the change |
| J1 53261-0271 | (33.0000, −84.9000), 90 | about (32.0500, −84.9000), 270 | read; offset estimated |
| J3 53261-0371 | (17.0000, −16.5000), 0 | about (17.0000, −15.3500), 0 | read; offset estimated |
| SW1, SW2, SW6 SKTDLDE010 | x 3.7600, 270 | about x 4.3100, 90 | read by eye; offset estimated |
| SW5 SKTDLDE010 | x 52.2400, 90 | about x 51.6900, 270 | read by eye; offset estimated |

The rows for U4 and J5, which Rev C changed against Rev B on the library's
evidence, are confirmed on this board: U4's pin 1 is on pad 1, and J5's
shell pegs are within 0.15 mm of their holes. With them, 105 previewed rows
are confirmed as committed. The nine "read" rows need a second preview with
the change applied before this file is corrected; the table for it is
[`../preview/corrections-next.csv`](../preview/corrections-next.csv).

## PCB order options

Unchanged from Rev B.

| Option | Main |
|---|---|
| Layers | 4 |
| Size | 53.43 × 92.43 mm |
| Thickness | **1.0 mm.** JLC's default is 1.6 mm, which would shift every case datum. |
| Stackup | **JLC04101H-3313**: 3313 prepreg 0.0994 mm + 0.7 mm core; 1 oz outer, 0.5 oz inner. The 50 Ω RF line on B.Cu (0.16 mm trace, 0.20 mm coplanar gaps over the In2 GND plane) is sized for this stackup. Do not substitute another stackup. |
| Surface finish | ENIG |
| Smallest via | 0.20 mm hole / 0.45 mm pad (all 477 vias) |
| Smallest track / gap | 0.12 / 0.10 mm |
| Via covering | **Epoxy filled & capped** (see below) |
| Order-number mark | **Remove order number.** The board has no "JLCJLCJLCJLC" placeholder; to use "Specify location", add one to the silkscreen first. |

The silkscreen reads "OpenWalkieTalkie main Rev B / 2026-10". That is
intended: the bare board is Rev B's.

**Why the vias must be filled and capped.** 48 vias sit in, or touch, SMD pads.
Open or tented vias there would wick solder:
- 16 GND vias in U5's exposed pad
- the thermal-pad vias of U6 (5), U7 (4) and U8 (4)
- vias in or at the pads of C1, C5, C7, C11, C12, C14/C15, C16, C39, R12,
  R27, R28, R29, U1, U20 and J5
- the CAL via in U5 pin 51, a 0.25 mm pad at 0.35 mm pitch
- the GND via beside the L6 shunt pad

## PCBA settings

- Assemble **both sides**: 125 parts on 51 BOM lines. 65 go on the bottom (B,
  the display side, which carries the MCU group) and 60 on the top (F, the
  battery side).
- Not assembled; these exist in the Gerbers only:
  - C52, C53, C58 and L6, the RF tuning shunts (value "NF", DNP, no paste)
  - R44, the 10k bench NTC substitute (DNP, no paste)
  - test pads TP1–TP10
  - mounting holes MH2 and MH4

  The compiler's supplier BOM and CPL list the DNP sites and test pads,
  because CoHDL has no population concept. `scripts/build_fab.py` removes
  them using `source/main/assembly.json` and stops if one reaches a file in
  this directory.
- CPL convention (same as Rev B and r6):
  - millimetres, KiCad's absolute origin (the Gerber origin), Y negated
  - bottom-side parts: X not mirrored, rotation = 180 − KiCad angle
  - top-side parts: rotation = KiCad angle
- Positions are footprint origins, except J5, which is at the library's
  mapped origin. `main-orientation-check.csv` lists, for every polarized part:
  - the JLC rotation, and whether it comes from a library binding or is the
    unverified convention angle
  - the footprint origin, and where pin 1 must land, seen from the top
  - the offset from the origin to the centre of the bounding box of all pads,
    locating holes included: J5 1.48 mm, J1/J3 0.60 mm, SW1/SW2/SW5/SW6
    0.55 mm, J2 0.10 mm, U3 0.03 mm, as on r6

  **Check every listed part in JLC's placement preview.** Every IC and the
  crystal has a silkscreen pin-1 mark: a dot, except U6, which has the
  footprint's corner triangle. Two library readings on other parts show that
  JLC's SOT-23-6 models do not all sit at the convention angle, so look
  closely at U2, U10 and U20.
- J5 (USB-C) has through-hole shell tabs, as on r6.
- USB-C and the side switches overhang the outline, as on r6, and there are
  no fiducials. JLC will add process rails; they must clear the overhangs.

## Parts that need a decision before ordering

Carried over from Rev B. Stock figures are Rev B's and were not refreshed.

| Line | What to do |
|---|---|
| Y1 E1SB48E001G00E (48 MHz crystal) | Not at LCSC/JLC (same as r6). Either use global sourcing, or review C54561124 (TKD SX20Y048000B81T001: 48 MHz, 8 pF, ±10 ppm, ESR not listed) against SiFli's limits (CL 7–12 pF, ESR ≤ 30 Ω). Leaving Y1 unmatched makes JLC ask about it at the BOM step. Do not let it go out as "do not place": the MCU has no 48 MHz reference without it. |
| J3 Molex 53261-0371 (LCSC C293630) | MX1.25 / PicoBlade 1.25 mm, 3 positions, SMT right angle. Check stock at upload. It mates with a Molex 51021-0300 housing. **The harness must be pin 1 VCELL, pin 2 NTC (10k 103AT, B ≈ 3435), pin 3 GND.** MX1.25 does not fix polarity and nothing on the board protects against a reversed pack. Without an NTC the charger will not charge; fit R44 for such packs. |
| AE1 2450AT18B100E, SW3/SW4 PTS636SM25SMTRLFS, U5 SF32LB525UC6, R32 604k, U6 BQ24073RGTR, U7/U8 RT6150BGQW, MK1 MP34DT05TR-A | Check JLC stock when the BOM is matched; the stock notes in the BOM are from 2026-09-23. |
| R37 3.6k RC0402FR-073K6L (LCSC C137990) | 0.5 C charge for the 500 mAh cell, 890 / 3.6k = 247 mA. About 590k in JLC stock on 2026-10-10. If it runs out, 3.57k (Yageo C137991 or UniOhm 0402WGF3571TCE C270609, 249 mA) or UniOhm 0402WGF3601TCE (C25891, 3.6k) also work; the charge current is 890 / R37. |
| C57 15 pF C0G (C1548), R40/R47 0R | The RF match ships as 15 pF series plus 0R / 0R, with the shunts open. Expect to tune S1/S2 (R40/R47) and the shunts (C52, C53, L6, C58) after a VNA measurement in the closed case. |

## After assembly (bring-up notes)

As for Rev B: the firmware is the `hw-b` build (amplifier enable on PA01,
CAL test pad TP3 on PA00, CHG on PA33), and the first RF check is BLE RSSI
and packet-error rate with the display streaming on and off. See
[`../../rev-b/README.md`](../../rev-b/README.md#firmware).
