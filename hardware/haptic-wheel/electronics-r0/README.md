# HW17 motor electronics R0

2026-10-30 · **Bench prototype, not a validated production release**

Two routed KiCad 10 boards for the custom three-phase, 12-slot / 14-pole wheel:

| Board | Construction | Function |
|---|---|---|
| HW17-driver-r0 | 50 × 40 mm, 2-layer FR4, 1.0 mm | DRV8311H, external 5 V supply, deck display-FPC interface |
| HW17-encoder-r0 | Ø5.5 mm central island, 1.6 mm flex tail | MT6701QT-STD absolute magnetic angle sensor, I²C |

The carrier stays outside the device for bench work. The encoder fits beneath the shaft magnet. The 31-pin OLED cable must be disconnected; the ordinary deck firmware cannot be used to drive this board. This package contains hardware and a firmware interface specification, **not motor-control firmware**.

## Files

- `source/HW17-driver-r0/HW17-driver-r0.kicad_pro`: carrier project; native schematic and routed PCB beside it.
- `source/HW17-encoder-r0/HW17-encoder-r0.kicad_pro`: encoder project.
- `source/HW17.pretty`: local footprints, including the unusual TI RRW-24 package and the host's actual FPC footprint.
- `fabrication/driver`, `fabrication/encoder`: Gerbers, Excellon drills, placement CSV, BOM and fabrication instructions.
- `review`: ERC/DRC/parity reports, mechanical check and visual review PDFs.
- `mechanical`: encoder STEP clearance envelopes and bare carrier STEP. The encoder model includes the stiffener, maximum chip and capacitor envelopes; copper, solder fillets and wires are not modeled. The carrier STEP is the bare PCB only.

Open the final projects named above. `-placed` and `-pre` files in the working repository are generation/routing intermediates, not fabrication masters. KiCad standard libraries are needed for library editing; the saved boards and schematics embed their geometry/symbols.

## Host interface

Verified against the actual Rev A r6 and Rev B main-board PCB netlists. J1 uses the same DIANWEI **8.03A5.031250**, 31-position, 0.3 mm pitch connector as host U3 (LCSC C55247264). Use a 0.2 mm FPC/FFC of the correct contact orientation. **Continuity-check every used pin end-to-end; do not infer pin mapping from the cable's appearance.**

| Host/display FPC pin → driver J1 | SF32 GPIO | Use |
|---|---|---|
| 4 | PA41 | Encoder SDA |
| 5 | PA42 | Encoder SCL |
| 8 | IO_3V3 rail | Sensor supply / logic pull-ups |
| 14 | PA9 | Driver enable through removable ARM jumper |
| 16 | PA5 | Phase U PWM |
| 17 | PA6 | Phase V PWM |
| 20 | PA7 | Phase W PWM |
| 21 | PA2 | nFAULT input, active low |
| 1, 7, 12, 22, 25, 28, 31, mounting pads | GND | Common signal / power reference |

All other FPC contacts are isolated on this carrier, including the display power rails and Rev B's pin-6 touch supply. PA5/6/7 support the SF32 timer pin-mux path; firmware must route three channels of one timer and configure synchronized PWM. Do not use three unrelated free-running timers. The stock display functions on these pins must be disabled first.

The host's 4-pin mechanical-encoder FPC is not used: it has two ground contacts and two filtered encoder inputs, with no supply contact. No phase-current ADC input is available on either host FPC. Therefore R0 supports angle feedback with **voltage-mode FOC**, not closed-loop phase-current regulation.

## External connections

| Connector | Pin order |
|---|---|
| J2, external supply | 1 = +5 V; 2 = GND |
| J3, sensor harness | 1 = GND; 2 = 3V3; 3 = SDA; 4 = SCL |
| J4, motor | 1 = U; 2 = V; 3 = W; star point stays inside motor |
| J5, ARM | 1 = enable command; 2 = driver nSLEEP; normally **no shunt** |
| J6, scope/external ADC | 1 = phase A current; 2 = B; 3 = C; 4 = CSAREF; 5 = GND |
| Encoder flex J1 lands | 1 = GND; **2 = SDA; 3 = SCL; 4 = VDD** |

**Sensor harness is not pin-for-pin:** carrier J3 → flex J1 is **1→1, 2→4, 3→2, 4→3**. Flex land numbering is in the KiCad board, placement drawing and wiring PDF. Land 1 is on the lower/left side of the tail when viewing the component side with the tail pointing upper-left. J1 on the flex is copper, not a purchased connector or pick-and-place part. Use short fine insulated wires, solder beyond the motor perimeter and strain-relieve them; do not attach a stiff cable directly to the sensing island.

## Driver circuit

- DRV8311**H**RRWR, hardware-interface variant; do not substitute P or S without redesign.
- MODE high: 3-PWM, 5 A driver overcurrent setting. INLA/B/C high enables all phases. GAIN high selects 2 V/A; SLEW low selects 35 V/µs.
- Separate external 5 V motor supply; SS14 reverse-input diode, SMAJ5.0A transient clamp, 100 µF bulk capacitor and local ceramics. VM is slightly below the external supply because of the diode. The TVS is not a continuous regenerative-energy sink.
- Driver AVDD is isolated from HOST_3V3. CSAREF uses AVDD through R12. Current-monitor nominal zero is CSAREF/2, with ±2 V/A response. Observe the datasheet's valid low-side conduction windows; these signals are not continuously valid DC phase-current measurements.
- 100 Ω series PWM resistors, 100 kΩ PWM pull-downs, 47 kΩ nSLEEP pull-down, and physical ARM isolation. nFAULT pulls up to host 3V3, not AVDD.
- The carrier is laid out for sub-amp bench experiments, **not 5 A continuous operation**. Some phase escape traces are 0.15 mm. The IC's 5 A OCP threshold does not protect the fine motor winding.

## Encoder and mechanical stack

MT6701QT-STD, QFN16 3 × 3 mm; MODE and CSN tied high for I²C. 7-bit address **0x06**. Read registers **0x03 then 0x04** in one transaction: `angle14 = (reg03 << 6) | (reg04 >> 2)`; mechanical radians = `angle14 * 2π / 16384`. There are 7 motor pole pairs; electrical angle requires measured direction and zero offset. Do not program EEPROM during normal testing.

The driver has 4.7 kΩ SDA/SCL pull-ups. With the host's existing 10 kΩ pull-ups the effective value is approximately 3.2 kΩ. Start at 100 kHz, verify edges, then try 400 kHz. Measure loop timing before choosing the final control bandwidth.

Mechanical coordinates use shaft center X=Y=0, carrier underside Z=0:

- Flex: nominal 0.15 mm finished thickness, Z=0.25…0.40 mm; tail width 1.6 mm, end radius 11 mm.
- Central PI stiffener including adhesive: nominal 0.25 mm, Ø5.5 mm. Finished island **0.40 mm maximum**; supplier must agree to a 0.36…0.40 mm stack and keep the top surface flat.
- Chip maximum 3.1 × 3.1 × 0.8 mm, plus 0.05 mm solder allowance: top Z=1.25 mm.
- 0201 capacitor body maximum 0.65 × 0.35 × 0.35 mm, center CAD (0, +2.25); check package substitutions against the Ø5 mm carrier bore.
- Shaft magnet: existing Ø4 × 0.5 mm, diametrical two-pole magnet, bottom Z=1.88 mm. Nominal chip-top gap **0.63 mm**. This gap does not establish adequate field strength or immunity to the motor magnets; those need measurement.
- Original HW17-R0 tail direction is +135°. The separate `-120deg.step` envelope is rotated −15° for the HW18-B1 carrier channel; rotate the complete PCB, then recalibrate encoder zero. The carrier files and hashes used in the interference checks are recorded in `review/mechanical-check.json`.
- Flex stack sits higher in the channel than the old generic rigid-flex tail placeholder, but remains within its 0.40 mm channel height. Nominal solid interference with both checked carriers is zero. Production tolerance and wire routing still need physical validation.

The MT6701 datasheet does not assign an electrical function to its exposed paddle. R0 solders it to an **isolated** copper island; obtain manufacturer confirmation of the paddle treatment before a production order.

## First-power sequence

1. Inspect QFN soldering, FPC orientation, flex wiring and resistance between all rails. Keep ARM shunt removed and motor disconnected.
2. Connect the deck with OLED unplugged. Load dedicated bench firmware that initializes nSLEEP low, PWM outputs safely, and nFAULT as an input. Do not fit ARM while the normal display firmware is running.
3. Verify encoder VDD and angle readings while turning the wheel by hand. Check full-turn monotonicity, repeatability and jumps with motor unpowered.
4. Connect the external 5 V supply with a low current limit, verify VM/AVDD, and scope PWM timing with motor still disconnected. Confirm enable and fault shutdown work.
5. Power off, connect U/V/W, restart in disabled state, then fit ARM only after bench firmware is ready. Start with brief, approximately **0.2 V phase-voltage commands**, using the measured VM in the modulation calculation. Monitor current and winding temperature. The supply current is not the same as phase current; supply limiting alone is not winding-current protection.
6. Measure encoder direction and electrical offset at low excitation. Compare angle readings with motor enabled/disabled before closing the position loop. Add command timeout, fault shutdown, voltage slew limits and integral limiting before haptic tests.
7. Increase excitation only from measurements. The R0 winding model estimates 2.15 Ω per phase and ~0.13 mm copper wire; it is not a measured rating. `Pcu ≈ 1.5 × Rphase × Ipeak²` gives ~0.40 W at 0.35 A peak. Validate the real resistance, inductance and temperature rise first.

No board has been fabricated, powered, or tested on hardware. No motor firmware, flash, release or commit was performed for this package.

## Verification and references

Final KiCad 10 checks: both schematics ERC 0; both boards DRC 0, unconnected 0, schematic parity issues 0. Checks include the true board-edge clearances and explicit isolated NC nets. No exclusions were used to suppress routing or parity errors. STEP solids are valid and survive round-trip export. These checks validate connectivity and nominal geometry, not motor performance or fabrication yield.

Primary component references:

- [TI DRV8311 datasheet](https://www.ti.com/lit/ds/symlink/drv8311.pdf): H-variant pin table, hardware straps, bypassing, RRW0024A land pattern.
- [MagnTek MT6701 Rev 1.8](https://www.magntek.com.cn/upload/pdf/202407/MT6701_Rev.1.8.pdf): QFN pinout, I²C reference circuit, angle registers and package limits.
- [SiFli SF32LB52x pin table](https://github.com/OpenSiFli/SiFli-SDK/blob/main/drivers/cmsis/sf32lb52x/bf0_pin_const.c): PA5/6/7 timer and PA41/42 I²C mux paths.

Generator scripts and manual-route recipes are retained for engineering changes. The reviewed routed `.kicad_pcb` files are the fabrication authority; do not regenerate them blindly, because generation creates new UUIDs and resets placement-stage sources.
