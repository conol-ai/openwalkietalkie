# OpenWalkieTalkie Rev B main board (BodyV6.13 case)

Rev B is a re-placed and fully re-routed main board, built from the
BodyV6.13 r6 release
([`../rev-a/pcb-release-bodyv6.13-20260923-r6`](../rev-a/pcb-release-bodyv6.13-20260923-r6/README.md)).
The mounting holes and left side switches are unchanged from r6. The outline is r6's, except the **bottom-right corner was rounded to match the top-left** (see Decisions below).

What moved:
- The MCU group is on the front (B, display) side, in the area the display
  covers.
- The antenna sits in the corner below the scroll wheel.
- The battery connector is a 3-pin MX1.25 for a cell with its own NTC.
- The audio path was placed and routed away from the flash and display buses.

The encoder boards did not change: use r6's `encoder/` or `encoder-wheel/`
board with this one.

Status (2026-10-06):

| Check | Result |
|---|---|
| KiCad 10.0.4 DRC, zones refilled, all severities | **0 violations, 0 unconnected** |
| Audio crosstalk check | **0 errors** (4 warnings, all explained below) |
| Placement check | 0 errors (1 proximity warning) |
| 3D fit against BodyV6.13 | No new interference |
| Netlist diff against r6 | Only the intended changes |

An independent six-part review was run and its findings were fixed or written
down (see [Verification](#verification)). **Before ordering, read
[Known limitations](#known-limitations) and [Firmware](#firmware).**

## Files

| What | Where |
|---|---|
| CoHDL source (electrical design and component placements) | [`source/main/src/main.cohdl`](source/main/src/main.cohdl); [build and parity check](source/README.md) |
| Routed board (KiCad 10.0.4, fabrication authority) | [`main/main-revb.kicad_pcb`](main/main-revb.kicad_pcb) (+ `.kicad_pro`, and `.kicad_dru` with the custom RF/audio rules) |
| Gerbers / drill / stencil | [`main/gerber/`](main/gerber/), [`main/drill/`](main/drill/), [`main/stencil/`](main/stencil/) |
| JLCPCB order package | [`fab-jlcpcb/`](fab-jlcpcb/ORDER-NOTES.md): Gerber + drill zip, BOM, CPL, orientation check list. **Read ORDER-NOTES first.** |
| Design BOM / KiCad positions | [`main/assembly/bom.csv`](main/assembly/bom.csv), [`main/assembly/positions-kicad-all.csv`](main/assembly/positions-kicad-all.csv) |
| Populated STEP | [`3d/main-revb-populated-bodyv6.13.step`](3d/main-revb-populated-bodyv6.13.step) (BodyV6.13 frame), [`3d/main-revb-populated-native.step`](3d/main-revb-populated-native.step), [`3d/bodyv6.13-revb-fit-assembly.step`](3d/bodyv6.13-revb-fit-assembly.step) (case + board + r6 encoder + display/battery envelopes) |
| 3D models | [`models/`](models/ATTRIBUTION.md) (the board references `${KIPRJMOD}/../models/`) |
| Verification reports | [`verification/`](verification/) |
| How the board was built | [`scripts/`](scripts/README.md) |
| UART first-flash jig (pogo plate for blank boards) | [`jig/`](jig/README.md) |

The aligned STEP uses r6's frame: X = u − 28, Z = 47.5 − v, Y along the board
normal. The F (battery) face is at Y 8.275 and the B face at Y 9.275.

## What changed, request by request

| # | Request | What the board does now |
|---|---|---|
| 1 | Charger I²C to the MCU | **Withdrawn**: the standalone BQ24073 stays. The r6 note "CHG to a GPIO" is done instead. The BQ24073's open-drain CHG goes to **PA33** (U5 pin 67), with R46 100k to IO_3V3, and it still reaches TP1. |
| 2 | MCU group on the display side, away from the battery | U5 is on B at (39.0, 24.5), inside the area the display covers. Also on B, all inside the panel outline: the flash U4 (32.6, 14.2), crystal Y1 (41.55, 31.0), buck inductor L3 and the supply caps. Five 0402 caps are on F, outside the battery reserve (v < 23.7): C23/C27/C28/C31 in a row about 3.5 mm north of U5's body (v 17.55), and C15 (AUDIO_REF) just off its east edge. The display FPC receptacle U3 moved from v 48.2 to v 36.75 so the display cable's U-turn clears the re-centred SW3. Tallest B part under the panel: U4, 2.2 mm, under the folded display flex, where 2.9 mm is allowed (3.3 mm elsewhere behind the panel). |
| 3 | Antenna near the scroll wheel | AE1 (Johanson 2450AT18B100E) is on F at (52.2, 24.6), rot 90, in the corner below the wheel. Its clearance is 42.1 mm² (Johanson asks for 42.25), copper-free on all four layers apart from the feed, and open on two board edges. Feed: U5 pin 53 → C57 15 pF (chip-side series; C52/C53 shunt pads DNP) → 50 Ω coplanar line on B (0.16 mm trace, 0.20 mm gaps over In2 GND, fenced) → via → R40 (S1) / L6 (P1, DNP) / R47 (S2) / C58 (P2, DNP) → AE1. S1/S2 are 0R until a VNA tune. The shunt pads have their own GND vias and plane voids underneath. Pin 53 to AE1 is about 20 mm (≈0.4 dB). |
| 4 | PTT and left/right switches centred on their buttons | SW5 (PTT) moved +5.0 mm to v 35.743, the PTT plate centre. SW3/SW4 (front left/right buttons) moved ∓6.52 mm to u 39.72 / 16.28, the cap-flange centres; r6 had them at the rounded outer ends. SW1/SW2/SW6 were already centred. The visible key-top centre is 0.35 mm from the flange centre; if that is the centre you meant, move SW3/SW4 by ∓0.35 mm. |
| 5 | Audio crosstalk with the flash SPI | **Placement:** the flash leaves U5's north face, and the DAC filter leaves the east face at 90° to it. The amplifier U1 is on F with R4/R5 at its input pins, so AMP_INP/AMP_INN are 1.5 mm long with no vias. Both inner layers are solid GND (r6's In2 carried the flash beside AMP_INN). **Routing:** fast nets keep ≥ 0.5 mm from the analog nets on the same layer and never run under them on an adjacent layer. In1/In2 cover 98–100 % of every audio track. The flash's 100 nF now sits at its own VCC pin instead of feeding through the audio-supply node. **Result** ([`verification/audio-xtalk.md`](verification/audio-xtalk.md)): 0 errors. Estimated coupling into the amplifier from fast nets is 0.01 fF·MΩ, against 307.8 fF·MΩ for r6 (about 90 dB less). |
| 6 | Battery with integrated NTC, MX1.25 3-pin connector | J3 is a **Molex 53261-0371** (PicoBlade/MX1.25, 3 positions, SMT right angle; mates with 51021-0300), in r6's J3 position. **Pin 1 CELL, pin 2 NTC → BQ24073 TS, pin 3 GND.** J4 (wire pads) is gone. The cell needs a **10k 103AT (B ≈ 3435) NTC**; without one, TS floats and charging stops. R44 10k (DNP) can stand in on the bench. |
| 7 | TP_VCC connected to 3V3 | U3 pin 6 (TP_VCC) → R48 0R → IO_3V3, decoupled by C20 at pin 8. Lift R48 to float the pin as on r6. |
| 8 | Charge the 500 mAh cell at 0.5 C (2026-10-10) | R37 (BQ24073 ISET) is **3.6k** (Yageo RC0402FR-073K6L, LCSC C137990) instead of r6's 2.55k: 890 / 3.6k = **247 mA**. The input limit stays USB500 (500 mA), so about 250 mA is left for the system before DPPM throttles charging (150 mA before). Termination is the chip's fixed 10 % of ISET, about 25 mA (0.05 C); the BQ24073 has no ITERM pin. Value-only change: copper, gerbers and the CPL are unchanged; BOM, position file, placement check and the 3D model name were updated. |

Carried over from the r6 notes:
- R4/R5 are now at the NS4150B input pins, so the high-impedance summing
  nodes are 1.5 mm long. Their value stays at r6's 22k: R_in does not set the
  r6 hiss, which was flash read bursts clicking into the summing node (see
  the rev-a README). The speaker level is the same as on r6.
- TOUCH_RESET has a 100k pull-down (R45), so the touch controller stays in
  reset while the MCU is asleep or off.

## Decisions and deviations to know about

- **MCU pin map changes.** Firmware must follow; see the next section.
  - AMP_ENABLE moved from PA20 (pin 25) to **PA01** (pin 50). That corner faces
    the amplifier; from the north face the line had no path out of the
    escape fields.
  - The CAL test pad TP3 moved from PA01 to **PA00** (pin 51, unused on r6).
  - CHG_STATUS is new on **PA33**.
  - PA20 is now unconnected.

  SiFli's production downloader and wired crystal calibration (Impeller) use a
  configurable "Signal Pin" that defaults to PA01: **set it to PA00 (TP3)**, or
  use OTA calibration. CAL's via sits in U5 pin 51's pad (filled and capped,
  see ORDER-NOTES).
- **Inner-layer exceptions.** Signals run on F and B, plus six short runs on
  the inner GND planes. Each plane fills as one piece. Apart from the two
  In1 supply links below, which always have solid In2 GND between them and
  the B-side copper, no inner trace runs under the RF line, the crystal, the
  DAC filter or the amplifier. A small In2 no-fill area at the R39 GND via (33.22, 30.0) keeps In2 from
  leaving a 0.1 mm² sliver between USB_MCU_P and USB_MCU_N there.
  - In1: MCU_VSYS (U5 pins 19 ↔ 52, 13.3 mm) and MCU_LDO1 (pins 22 ↔ 38,
    8.0 mm), under U5; MCU_LDO1 continues 2.4 mm east of the package to its
    via at (44.65, 21.95). Any F/B path would cut through an escape field.
    r6 ran MCU_VSYS on In2. MCU_VSYS crosses under BT_RF/XO/XI at the pin
    row, and MCU_LDO1 under DAC resistor R1 and the DAC pair's first
    millimetre. Both are quiet supplies (MCU_LDO1 is also the audio supply).
  - In2: ENC_B 9.0 mm, USB_MCU_N 7.0 mm, PANEL_ENABLE 6.9 mm and USB_MCU_P
    4.8 mm.
- **0603 → 0402.** C13, C14, C15, C16, C17, C23, C24, C26, C27, C28, C29, C30
  and C31 changed to 0402 to fit the escape fields around the relocated MCU.
  C28 sits on SYSTEM_SUPPLY (up to 4.4 V). Its 10 V X5R part keeps 2× voltage
  margin but loses roughly half its capacitance to DC bias.
- **Debug / test pads** TP3/TP4/TP5/TP6/TP8/TP10 moved to the F side at
  (37.5–41.9, 47–49). That is outside the battery reserve, so they are
  reachable from the back. The pattern is r6's rotated 180°; the pitch is
  unchanged.
- **Bottom-right corner rounded.** The board's bottom-right corner was changed
  to match the top-left: its sharp diagonal cut-ins were replaced by the same
  smooth tangent fillets, keeping the 2.0 mm case locating lobe. The change only
  removes copper-free board material (≈3 mm²); it does not move any part, net or
  track (BOM/CPL/netlist unchanged), and DRC stays 0/0. The board still fits the
  BodyV6.13 case (it only pulls the edge inward there), but note the case's
  bottom-right pocket wall is **not** symmetric with the top-left, so the board
  now sits with a small gap (up to ~2 mm) at that corner rather than hugging the
  wall; matching the case cosmetically would be a separate Fusion edit.
- **Board label.** "OpenWalkieTalkie main Rev B / 2026-10" is on the F silkscreen,
  and the title block says revision B. The outline is r6's apart from the
  rounded bottom-right corner below, so the silk/title block is the main way to
  tell the boards apart.

## Known limitations

1. **Display data lines PANEL_D1/PANEL_D3 run beside the antenna ground edge.**
   - Their copper is 0.31 mm (D1) and 0.42 mm (D3) from the antenna clearance;
     the antenna guidance asks for 3 mm. The first release candidate had them
     at 0.04 mm.
   - Apart from the RF feed, GND and the DAC pair (limitation 2), the nearest
     other copper is PANEL_RESET at R30, 2.38 mm away. Everything else is
     ≥ 3.1 mm away ([`verification/final-checks.txt`](verification/final-checks.txt)).
   - The panel bus could not be fitted west of the DAC pair with the other
     lines in place.
   - Risk: display-refresh harmonics injected at the antenna's ground edge
     during BLE use.
   - **Test it on the first build:** measure BLE RSSI and packet-error rate
     with the display streaming on and off.
   - A future revision should move the DAC pair or reorder the panel escape
     to free that corridor.
2. **The DAC filter pair runs 0.9–2 mm from the antenna clearance** on its way
   south, a consequence of the antenna corner. Risk: BLE TX bursts rectified
   at the amplifier input as a buzz. The amplifier input has no spare RF
   shunt-cap footprint. If a connection-interval buzz shows up, 33–100 pF to
   GND at C3/C4 is the fix (bodge, then a footprint next revision).
3. **XO and the RF line.** The crystal's XO pad is 0.43 mm from C53 (the RF
   line after C57), with no ground between. At the pins, the XO trace passes
   0.165 mm from U5 pad 53 (BT_RF) and runs 0.21 mm from the 0.8 mm BT_RF
   stub beside it; this is inherent to adjacent pins 53/54 and similar
   to r6. The 51st harmonic of 48 MHz lands at 2448 MHz.
4. **U5 VCC (pin 24) decoupling.** C28 (4.7 µF) is 4 mm of trace from the pin,
   through one via. The trace is 0.12 mm through the pin field and the first
   1.2 mm on F, then 0.2 mm. On r6 it was about 0.7 mm. SiFli asks for the cap
   close to the pin and a trace ≥ 0.4 mm. The B side has no room for a second
   cap at the pin.
5. **Display connector bypass.** C20 is 5.6 mm and C21 8.5 mm from their U3
   pins, against about 1–2 mm on r6. The panel FPC carries its own
   decoupling. PANEL_4V's C19 improved from 10.8 to 1.4 mm.
6. **USB D+/D-** are routed separately (25.7 / 36.3 mm, 2 vias each). That is
   harmless at full speed (65 ps of skew against an 83 ns bit), and r6 ran
   them more than 90 mm.
7. **Y1 (48 MHz crystal)** still has no LCSC part, as on r6. See ORDER-NOTES.
8. **Charger input limit.** As on r6, the BQ24073 has EN1 high and EN2 low
   (USB500). The input current is capped at 500 mA whatever the USB-C source
   offers, and R38 (ILIM) is not used in this mode. Charge current is
   ISET-set at about 250 mA (R37 3.6k: 890 / 3.6k = 247 mA, 0.5 C of the
   500 mAh cell; changed 2026-10-10 from r6's 2.55k, 349 mA). Termination
   is fixed at 10 % of that (about 25 mA, 0.05 C): the BQ24073 has no ITERM
   pin. Under heavy system load (speaker at full volume), DPPM takes current
   back from charging first. The no-cell brownout at max volume is partly
   this limit. Allowing more needs ILIM mode (EN2 high, EN1 low) and a
   smaller R38 (its 3.3k would set only about 470 mA) in a future revision.
9. **Audio checker warnings.**
   - AC/AMP_VO/SPEAKER pairs are wider than the checker likes. This is set by
     pin and pad spacing.
   - AVDD33_AUD shares MCU_LDO1 with the flash VCC. This is a schematic choice
     inherited from r6; the flash now has its own bypass right at its pin.

## Firmware

Rev B is the firmware's `hw-b` board: `--features hw-b` for the Embassy
image, `OWT_BOARD=hw-b tools/build-native.sh` for the native one (which
defines `OWT_BOARD_HW_B` on the C side). `hw-b` implies `hw-a`, the main
board as such, and overlays the three pins that moved:

- **AMP_ENABLE PA20 → PA01.** `src/board.rs` hands the NS4150B's CTRL pin
  to `src/audio/speaker.rs` (which takes any pin); `native/src/audio.c`
  drives `OWT_AMP_PIN` and `pinmux.c` sets PA01 as a pull-down GPIO. PA20
  stays a pull-down.
- **CAL / TP3 on PA00.** Parked as a pull-down input by both images. Set
  SiFli's calibration tool's signal pin to PA00.
- **CHG_STATUS on PA33.** Both images read it (R46 is the pull-up; low =
  charging). The Embassy image combines it with its USB link, because the
  MCU's VBUS pin is not wired: low is *charging*; high with a USB host and
  the cell within 80 mV of 4.2 V is a termination, which the battery
  estimator confirms as 100 % after three samples over 30 s; high with a
  host and a lower voltage is a charger that stopped for another reason
  (its NTC window, its timer); high without a host is no input, so a charge
  that ends on a charger-only supply holds the last figure. The native
  image has no USB device and only tells charging from not charging. PA33
  is not a wake pin: PMU slot 1 is free since CANCEL stopped waking the
  deck, but a charger event waking it would only meet the 2 s PTT power-on
  gate and go back to sleep.
- **Board name.** Settings → About reads `Rev B`, the boot log `board:
  OpenWalkieTalkie Rev B (BodyV6.13)` (native: `board: hw-b image`),
  `sys.version` answers `"board": "Rev B"`, and `tools/flash-usb.py`
  refuses a Rev A image for a deck that reports Rev B (and the reverse)
  unless `--any-board` is passed. A `hw-a` image boots on Rev B with its
  speaker silent and no charge state.

First flash: `tools/flash-new-board.sh` with the `hw-b` images
(`OWT_BOARD=hw-b OWT_BOOT_IMAGES=1 tools/build-native.sh` for the flash
table and bootloader, `cargo build --release --features hw-b` for the
application), over the pads the [jig](jig/README.md) lands on. After that
the application updates over USB-C.

To check on the first board:
- **Touch in soft-off.** U3 pin 6 is now powered, so the CST9220 runs from
  IO_3V3, which stays on in soft-off; R45 holds it in reset while PA44 is
  undriven (the HPSYS pads power down with the core). Measure the soft-off
  current with a cell connected; put the controller to sleep before the PMU
  shutdown if it is too high.
- **Speaker.** The amplifier enables on PA01 (the native image logs
  `speaker: enabled PA1`); the Rev A hiss fix (code run from PSRAM) applies
  unchanged.
- **Charging.** With the Mac on the cable, Settings → Battery shows
  *charging* and later *full*; on a charger-only brick the figure holds at
  the end of the charge.
- **RF.** PANEL_D1/D3 run close to the antenna clearance; test BLE RSSI and
  packet loss with the display streaming.

## Verification

All reports are in [`verification/`](verification/).

- `main-revb-drc.rpt` / `.json`: KiCad 10.0.4 DRC with zones refilled, all
  severities: **0 violations, 0 unconnected**. The custom rules in
  `main-revb.kicad_dru`:
  - RF50 lines keep 0.2 mm except between tracks/pads inside U5's pin field.
  - AudioAnalog nets keep 0.3 mm from Default-class copper.
  - The amplifier summing nodes keep 0.5 mm from all non-audio, non-GND copper.
- `audio-xtalk.md` / `.json`: 0 errors, 4 warnings (see Known limitations).
- `placement-check.md`: 0 errors. The 1 warning is R6, the PDM-clock series
  resistor, at 4.1 mm from its pin against a 4.0 mm target.
- `bodyv6.13-fit-model-report.json`: 3D fit of every modelled part against
  the case, the r6 encoder STEP, and the display, battery and FPC envelopes.
  - The only contacts are the ones r6 already had: J5 USB-C 0.16 mm³ in the
    case mouth corners, and the side-switch actuators 0.06 mm from their
    button plates.
  - No part-to-part or part-to-board interference.
- `final-checks.txt`: copper distances to the antenna clearance, other-net
  vias near the crystal (nearest: TOUCH_RESET, 0.78 mm edge to edge from a Y1
  pad), inner-layer tracks per net, via count.
- `netlist-diff-vs-r6.txt`: pad-level net diff against r6. It contains
  exactly the intended changes: J4 removed; C57, C58, L6, R44–R48 added;
  J3 pin 2/3; U3 pin 6; U5 pins 25/50/51/67; the RF chain; values C52/C53
  (now NF, DNP). R4/R5 keep r6's 22k.
- [`review-2026-10-05.md`](verification/review-2026-10-05.md): the independent
  review (mechanical, RF, audio, electrical, DFM, completeness), with what was
  fixed and what remains.

## How it was built

The board was generated by scripts from the r6 release:
1. Netlist and part changes.
2. Region placement, checked against the case envelopes.
3. Hand-routed skeleton: MCU escapes, RF, crystal, audio, power, supply
   links.
4. Two staged Freerouting passes, finished by a purpose-built grid router with
   rip-up.
5. GND pours and stitching.
6. Post-review fixes.

[`scripts/README.md`](scripts/README.md) lists the historical layout steps.
The restored [`source/main/`](source/README.md) project was copied from r6
and reconciled to the released Rev B PCB, including r6's previously missing
microphone, charger and adjustable IO-regulator changes. It preserves the
released designators, component placements and outline. CoHDL supplies the
circuit and part bindings; `source/main/assembly.json` supplies DNP population.

Run `./hardware/rev-b/check.sh` from the repository root to compile the source
and compare it with the released PCB and design BOM. The recorded result is
[`verification/source-parity.json`](verification/source-parity.json).
The released `main/main-revb.kicad_pcb` remains the fabrication authority for
routing, planes, stackup, rules and manufacturing details that the compiler
does not reproduce. Future electrical edits must update both source and PCB
and pass the parity check.

## License

The design files in this directory (the KiCad board and jig projects,
fabrication outputs, BOMs, placement files, 3D fit models and this
documentation) are CERN-OHL-P-2.0, see [`../LICENSE`](../LICENSE) and
[`../README.md`](../README.md). Everything under `scripts/` and `jig/scripts/`
is software and stays MIT like the rest of the repository. Third-party models
keep their own licenses, see `models/ATTRIBUTION.md`.
