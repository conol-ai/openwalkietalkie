# 38 mm display cable: dimensioned extraction and conditional fold study

Read-only study of the user-specified **FPC3-H0175Y003MT006-1 V0**. No board, component, source or case geometry was changed. `cable-fold-review.json` records source hashes, DXF handles and parameterized calculations; `extract_fold.py` reproduces it using the existing geometry Python runtime.

## What the DXF actually establishes

| Feature | Dimension and evidence |
|---|---|
| Cable outline | Nominal **38 × 9.6 mm**; outline `236D9` measures 37.998451 × 9.600000 mm. The title explicitly says mm despite `$INSUNITS=0`. |
| Thick/hatched ends | **3.5 ±0.3 mm** at each end (`236DA`, `236DD`, dimension `236EE`). The exact end reinforcement material is not assigned by this view. |
| White alignment marks | **2.5 ±0.3 mm** from each tip (`236DB`, `236DE`). This alone does not certify a socket insertion depth. |
| Explicit single-layer bend area | `236FE`/`236FD`, labelled **弯折单层区**: **s=4.500000…33.498451 mm** from one tip, length 28.998451 mm, width **9.2 mm**, inset 0.2 mm from the sides. |
| Thickness | Side view depicts **0.10 mm** through the flexible span and explicitly dimensions **0.20 ±0.03 mm** at the ends. No separate central-thickness tolerance is dimensioned. |
| Bending qualification note | `15990` specifies an **R0.5 mm steel rod**, 180° reciprocal bends, over 50 cycles, ±180 counted as one cycle. This is not permission for a zero-radius crease or a claim of unlimited cyclic life. |

The general material table lists, in order: Ni 2–6 / Au 0.05–0.1 / EMI 15 / coverlay PI 12.5 + adhesive 20 / Cu plating 9–18 / FCCL Cu 12 + adhesive 0 + PI 20 + adhesive 0 + Cu 12 / Cu plating 9–18 / adhesive 20 + PI 12.5 / EMI 15 / Au 0.05–0.1 / Ni 2–6, all in µm. Their arithmetic sum is **161.1–187.2 µm**. Do **not** assign this full two-copper-layer stack to the explicitly single-layer central bend region; the drawing does not map every table row by region. Default unmarked dimensional tolerance is ±0.10 mm.

## Alignment and a useful provisional PCB position

The current BodyV5 `Display` sketch fixes the nominal cover-glass centre at **X=0, Z=20.305 mm**: its 44.09 × 45.39 mm outline extends from Z−2.39 to Z43.0. The sketch's Y15 plane is not proof of the assembled connector mating-plane height.

Using that centre and the module drawing's actual 31 pad rectangles, the panel-side land-array centre is **board (u,v)=(36.586905,30.534537) mm**. The cached connector INSERT bounding box includes numbering text and must not be used as a precise housing/mating datum. The rectangle set itself does not dimension the socket mouth.

The prior photograph gives cable lane **u≈37.216 mm** near v48.2. Its 0.63 mm difference from the nominal module axis is credible photo/loose-flex parallax, not a reason to discard the dimensioned land-array result. Treat photo accuracy as roughly 1–2 mm or worse, not the small fitted fiducial residual.

Thus **B-side U3 near u36.6, v48.2** is a useful provisional routing/layout candidate, subject to the separate contact/orientation review and actual mouth offsets. It is not a fit-certified pose. A side-only flip at current u26.345 leaves a **10.241905 mm nominal lateral error**. A conventional transverse 180° U-fold preserves its lateral lane; it does not remove that error.

The authoritative corrected assembly frame has **B facing +Y toward the display**, finished B surface Y9.275; F faces −Y toward the battery. Therefore this B-side cable route **does not need to wrap around the PCB edge**. The older opposite-side interpretation has been discarded.

## Length capacity versus installed clearance

For an ideal U consisting of two parallel straight legs and a 180° circular bend, let R be neutral-axis radius, a/b the two insertions, and D the longitudinal separation of the mouths:

`A=(L-a-b-πR+D)/2`, `B=(L-a-b-πR-D)/2`; the bend occupies cable interval `[a+A, a+A+πR]`. Parallel-leg height separation is `2R`; total outer loop height is `2R+t` for flex thickness t.

With **assumed** 2.5 mm insertions, t0.10 and inner radius0.50 (neutral R0.55), the free cable is 32.998451 mm, arc length **1.727876 mm**, and outer loop height **1.20 mm**. Using source land-array v30.534537 as a deliberately approximate mouth reference and receiving mouth v48.2 gives outgoing/return legs **24.468019 / 6.802556 mm**. Bend s26.968019…28.695895 is comfortably inside the explicit single-layer region. Its outer longitudinal apex is **v55.602556**. Substituting the first visible photo-flex reference v34.571 gives apex **v57.620797**. These reference choices bracket a study, not a claimed measurement of either socket mouth.

Both turns project below the large macro cap at u28.2…51.245, v52.09…62.09. This is not automatically a collision: the cable can pass underneath the cap, but its **moving** underside matters. The corrected B-mounted switches yield cap underside **Y11.50976** at the upper stated switch travel of 0.35 mm. For a chosen 0.30 mm assembly clearance, keep cable upper surface **≤Y11.20976** in that swept cap region. Static cap underside Y12.8 is an inadequate clearance test.

**A pure constant-height-leg U-fold cannot be certified from these drawings**, because the display and PCB socket mating heights are not dimensioned. If the outgoing display-side leg is too high, it needs a gentle height drop *before v52.09*, or a revised longitudinal placement. The cable length and the 29 mm single-layer region allow investigation of such a drop; they do not prove the real parts clear it.

### Parameterized height-drop option, not an invented mating height

Two opposite equal-radius arcs can connect parallel tangents with vertical drop δ. For neutral radius Rs:

`θ=acos(1−δ/(2Rs))`, horizontal run `2Rs·sinθ`, arc length `2Rs·θ` (0≤δ≤2Rs).

For example **δ=1.0 mm, Rs=2.0 mm** requires 2.645751 mm of longitudinal run and 2.890937 mm of cable, only **0.245186 mm** more than a straight run. These are illustrative free parameters, not measured socket heights. Such a transition can be positioned within the first long leg and within s4.5…33.498, terminating before the moving-cap region, then followed by a lower U-loop. Both transitions must be recomputed from actual mouth planes and checked for nearby components, minimum bend radius, cap travel, insulation and latch access. No certified 3D cable curve is delivered.

## Switch-frame cross-check requested during this study

`switch-frame-review.json` and `switch-frame-review.md` separately record the read-only checks. In summary:

- Existing F-side SW3/SW4 face away from the macro caps. The approved B-side correction restores the actuation direction at unchanged X/Z.
- On B at the supplied solder seating datum Y9.30976, the normalized PTS model's 2.55 mm plunger top is Y11.85976. Cap underside Y12.8 leaves **0.94024 mm free movement**; adding nominal 0.25 mm switch travel puts the cap top at **Y15.00976**, nearly flush with the case's Y15 front. The gap alone is therefore not evidence of a bad B-side placement. Travel/tolerance, loose retention and repeatable contact still need real assembly validation.
- The four SKTD side switches retain the appropriate F side: their actuator/cap side gap is nominally **0.06 mm**, with their actual actuator heights inside the broad cap rear contact plates. A 0.15 mm switch stroke corresponds to about **0.21 mm cap movement**, before tolerances.

## Remaining limits

No full display model or mating-plane Z/Y dimension has been supplied by these two drawings. Contact-side/pin mapping is handled independently. The source cap motion and nominal switch geometry do not qualify solder height tolerance, printed dimensional error, cap retention, lateral wobble, force, overtravel stops, or wear. The cable part has a different part-number stem from the panel, although the user explicitly confirmed this cable size; mechanical agreement does not itself establish electrical interchangeability.
