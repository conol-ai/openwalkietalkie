# Corrected-frame switch/cap check

Read-only measurements against BodyV5 v8, the current native PCB, and the normalized vendor/community STEP sources. Details and exact contact-face checks are in `switch-frame-review.json`; `check_switch_frame.py` reproduces the calculations. No PCB or case was changed here.

The authoritative assembly frame is **B toward display (+Y), F toward battery (−Y)**. Finished B is Y9.275, and the supplied native model solder-seat datum is Y9.30976. The corresponding F datum is Y8.21056.

## Macro switches: unchanged X/Z, corrected to B0

SW3 and SW4's 2.7 × 1.3 mm actuator-top rectangles lie completely within their actual cap rear-face polygons. The B-mounted normalized PTS model puts the unpressed plunger at **Y11.85976**. Cap underside is Y12.8, leaving **0.94024 mm free cap movement** before contact.

| Switch travel | Total cap movement | Pressed cap front Y | Position relative to case Y15 |
|---|---|---|---|
| 0.15 mm | 1.09024 mm | 15.10976 | 0.10976 mm proud |
| 0.25 mm nominal | 1.19024 mm | 15.00976 | 0.00976 mm proud |
| 0.35 mm | 1.29024 mm | 14.90976 | 0.09024 mm recessed |

The large initial gap is therefore compatible with a cap that starts 1.2 mm proud and becomes approximately flush on actuation. It is not, by itself, evidence that B0/current XY is wrong.

No nominal fixed-case obstruction was found along this inward translation. The narrow cap stem has **0.20 mm nominal lateral clearance** inside the case opening. Its larger flange descends away from the case's Y14 underside: maximum-stroke flange sweep is Y11.50976…13.8. The only maincase face whose bounding box meets the nearby flange sweep is MH3 boss133; the conservative full-circle planar separation from the cap flange is **1.033909 mm**.

This is a source-boundary/alignment check, not a tolerance-qualified native assembly sweep. The 0.94 mm free movement may affect rattle, feel and retention; those require physical validation. The community PTS CAD is 2.55 mm tall after explicit solder-seat normalization, versus vendor nominal 2.5 mm. No height-tolerance distribution is inferred.

## Side switches: retain F

Actual normalized SKTD actuator front face417 is at physical local y−1.5, with z0.495…1.102309. Applying the existing native ±90° F rotations gives:

| Switches | Actuator X | Cap rear X | Nominal gap |
|---|---|---|---|
| SW1 / SW2 / SW6 | −25.74 | −25.8 | 0.06 mm |
| SW5 | +25.74 | +25.8 | 0.06 mm |

Their actual actuator-front rectangles lie inside the cap rear polygons, with minimum planar edge margins **3.08565 mm** for the round caps and **4.60825 mm** for PTT. Actuator Y is **7.10825…7.71556**, so the corrected frame does not invert or miss these side-contact plates. SKTD nominal 0.15 mm switch travel plus the 0.06 mm gap gives approximately **0.21 mm inward cap movement**. Whole sidecap/retention sweeps and printing/solder tolerances remain unqualified.

## Cable consequence

The macro cap's underside can reach **Y11.50976** at 0.35 mm switch travel. A selected 0.30 mm clearance means cable upper surface must be **≤Y11.20976** beneath the swept cap region. Testing only against the static underside Y12.8 would miss a possible pressed-button/cable collision. The FPC report therefore retains a conditional gentle height-drop option and does not certify a pure U-loop from unknown connector mating heights.
