# OCN entry direction and source pin mapping — drawing resolution

Read-only review of OCN `OK-F302-**115` p1/p3, the named FPC3 DXF, current source footprint and case photo. No pad geometry or numbering changes were made.

## Directional result

The current source comment “local +y is provisionally the cable-entry side” is reversed relative to the OCN mechanical drawing. For the existing local lands, the connector entry is **authoring −Y**, which is **normalized component-model CAD +Y** (`CAD y = −authoring y`).

Evidence chain:

1. The OCN package's upper-left plan shows the actuator/front at the lower edge and exposed rear terminal tails at the upper edge. The rear tail row is explicitly dimensioned **B**; its span is8.40mm for31 contacts.
2. The underside plan below it explicitly dimensions the other/front row **C**; C is9.00mm for31 contacts. The asymmetric row spans fix which row is front/rear without relying on a pin1 convention or on the unlabelled view's screen direction.
3. Section A-A shows the mating/contact opening on the left, the front-flip actuator opening above it, and the rear body/tail on the right. This agrees with the actuator/front being the C side and the exposed narrower B row being rear. The shown reference2.15mm dimension runs inward from the front opening; the1.85mm open height is separate.
4. The manufacturer's recommended **TOP VIEW** PCB pattern puts C on its upper row of short0.20×0.55mm pads and B on its lower row of long0.20×0.75mm pads. This pattern is oriented180° relative to the package upper-left plan. Do not simply copy the package plan's page-down front direction onto the separately oriented recommended pattern.
5. The existing source exactly maps that recommended pattern: C/short row at authoring y=−1.425, B/long row at y=+1.325. Consequently the physical entry faces authoring −Y.

The direction is a geometric inference from mutually consistent manufacturer views, not a new manufacturer text label; the traceable asymmetry is B versus C. The generic illustrated package is marked25, so use the31-contact table row for counts/spans rather than counting the illustrative drawing alone.

## Odd/even row and pin1

For31 positions at0.30mm pitch, the outside/every-other set has16 positions at x=−4.5,−3.9,…,+4.5 (C span9.0), while the interleaved set has15 at x=−4.2,…,+4.2 (B span8.4). In the FPC3 DXF, the outside contact on one edge is labelled1 and the outside contact on the other31; therefore the16-position C row is the odd row and B is even. This supports the existing source row assignment, not swapping short and long rows.

The DXF's reverse/stiffener view labels pin1 on the left at **both** ends, while its exposed-contact view has pin1 on the right at both ends. For top-side viewing of a bottom-contact receptacle, the cable's stiffener/back face is visible. With the entry at authoring−Y and the cable extending in that direction, its lower pictured end enters the receptacle. Pin1 therefore lands at local−X, matching the source's current pin1 at(−4.5,−1.425). No physical copper re-numbering is indicated by these drawings.

The connector need not have a visible moulded pin1 marking to use this mapping: it is a position-based assignment of the cable's specified pin1 to the matching contact lane. This remains drawing-level verification; the DXF does not provide a continuity test of the supplied cable or a detailed internal connector netlist. It also assumes the supplied cable is this FPC3 design, beyond matching dimensions alone.

## Proposed B-side U-fold transform

For a cable returning upward after a transverse180° U-fold, with connector mouth facing board+v/down:

- Keep the current local pad numbering and geometry.
- Use **CoHDL `side bottom rotate180`** for U3 (before any translation required by the layout).
- CoHDL's mirror-local-x then rotate180 produces physical point `(x,y) → (x,−y)`. Thus local pin1 stays at the board-left edge, and local entry vector `(0,−1)` becomes board `(0,+1)`.
- This is **native KiCad B.Cu orientation0°**, not native180°. Native B.Cu180° corresponds to CoHDL bottom rotate0, would place pin1 on the board-right edge and entry toward board−v/up for this corrected local direction.
- A transverse fold reverses the cable contact normal but does not swap its left/right pin1 edge. Do not reverse1↔31 to compensate for a wrong in-plane orientation.

## Narrow source/model metadata changes recommended

Change only the provisional entry-direction assertion to authoring−Y, normalized CAD+Y; record the B/C table and section evidence. Record C=odd, B=even and pin1 local−X as the drawing-derived mapping for this FPC3. Leave actual part procurement/revision/sample-identity and dimensional-body-profile caveats explicit. If retaining the existing `_PROVISIONAL` footprint name avoids an unnecessary source identity churn, document which aspects are now resolved rather than renaming geometry during a routing-only change.

The earlier reusable `component-model-specs.json` OCN entry vector `[0,−1,0]` in normalized CAD was provisional and is superseded by **`[0,+1,0]`** here. Its centred conservative body envelope remains an approximation; this direction resolution is not a new exact vendor STEP profile or a complete installation/access clearance proof.
