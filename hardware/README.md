# OpenWalkieTalkie hardware

Board, case and fixture designs for the OpenWalkieTalkie deck. `rev-a/` holds
the BodyV5/BodyV6 lineage (r3 to r6) together with the case CAD and component
evidence; `rev-b/` is the current re-placed main board and its UART flash jig;
`rev-c/` is the same board on current CoHDL and library versions, with the
order data produced through CoHDL supplier bindings (copper unchanged).

## License

Copyright (c) 2026 Tony Huang.

This source describes Open Hardware and is licensed under the CERN-OHL-P v2.
You may redistribute and modify this documentation and make products using it
under the terms of the CERN-OHL-P v2 (https://ohwr.org/cern_ohl_p_v2.txt).
This documentation is distributed WITHOUT ANY EXPRESS OR IMPLIED WARRANTY,
INCLUDING OF MERCHANTABILITY, SATISFACTORY QUALITY AND FITNESS FOR A
PARTICULAR PURPOSE. Please see the CERN-OHL-P v2 for applicable conditions.

The full text is in [LICENSE](LICENSE); the SPDX identifier is
`CERN-OHL-P-2.0`. It covers everything that describes the device itself: the
CoHDL schematic sources, KiCad schematics and layouts, Gerbers and drill files,
bills of materials and placement files, the case and fixture CAD (`.f3d`,
`.step`, `.dxf`), fit assemblies, renders, drawings, and the release notes that
document them. It is a permissive licence: you may build, modify and sell
products from these files, including commercially, as long as you keep the
copyright and licence notices and do not use the project's names or marks to
imply endorsement.

It does not cover:

- **Software.** The scripts and tooling in this tree (`rev-a/check.sh`,
  `rev-a/validate_release.py` and every `scripts/` directory) are software and
  stay under the repository's MIT license, see [../LICENSE](../LICENSE). Each
  file carries an `SPDX-License-Identifier: MIT` header.
- **Third-party material.** Vendor CAD, datasheets, drawings and KiCad library
  models keep their own copyrights and licenses. See each release's
  `models/ATTRIBUTION.md`, `rev-a/mechanical-source/vendor/` and the
  provenance manifests next to them. Nothing here relicenses them.
