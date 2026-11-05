# How the Rev B board was built

The maintained CoHDL project is now in [`../source/main/`](../source/README.md).
`../check.sh` runs `build_source.py` and `check_source.py` to compile it,
apply population metadata, and compare the result against the released PCB
and BOM. These commands write only generated files under `source/main/out/`.

The remaining scripts below document the original layout process.

These are the scripts that produced `../main/main-revb.kicad_pcb` from the r6
release. They record exactly how the design was made. They are not a
push-button rebuild:
- They use absolute paths into the build session's scratch directory. Edit the
  `S` / `RB` constants first.
- Freerouting is not deterministic, so a re-run gives a different (but
  equivalent) autorouted part.

The released board file is the authority, not these scripts.

Tools: KiCad 10.0.4 (its bundled python3.9 with `pcbnew` and numpy, and
`kicad-cli`), Freerouting 2.4.1 (headless, through `tooling/fr_route.py`), and
a python 3.12 with shapely/matplotlib/build123d (`geopy`) for the placement
and 3D checks.

## Order of operations

1. `setup_revb.py`: r6 board → `main/main-revb.kicad_pcb`. All netlist and part
   changes happen here:
   - J3 becomes the 3-pin Molex 53261-0371; J4 is deleted.
   - New parts: R44 (DNP), R45, R46, R48, and the RF chain (C57, L6, R47, C58).
   - R4/R5 change to 100k. This was reverted to r6's 22k after release
     (`r4r5_22k.py`, [`postreview/`](postreview/README.md) step 12).
   - TP_VCC is tied to IO_3V3 through R48.
   - CHG goes to PA33.
   - AMP_ENABLE moves to PA01 and CAL to PA00.
   - In2 becomes a GND plane.
   - All r6 copper is removed.
2. `place_revb.py`: runs setup, swaps the relocated MCU-group capacitors to
   0402, then places parts per region (`regions/{mcu,power,topleft,audio}.py`).
   `check_place.py` (via `extract_place.py` + `geopy`) checks regions, case
   areas, battery reserve, proximity rules and pad clashes. Result: 0 errors.
3. `board_rules.py`: In2 set to plane, rule areas (antenna keepouts, cable
   lane, plug withdrawal, case contact areas), net classes (RF50, AudioAnalog,
   McuPower) and the custom `.kicad_dru` rules.
4. `route_revb.py` with the hand-routed modules in `route/`, all locked:
   - `mcu.py`: MCU-critical copper — EP vias, RF stub, crystal, DAC filter,
     AUDIO_REF, VSYS.
   - `rf.py`: 50 Ω coplanar line, antenna network and fence.
   - `audio.py`: amplifier, R4/R5 at its pins, DAC pair.
   - `power.py`: USB, charger, regulators, rails.
   - `mcu_esc.py` / `esc2.py`: via fan-outs on all four QFN faces, display
     connector vias, touch/I²C links.
   - `links.py`: MCU_VSYS / MCU_LDO1 links on In1, CELL sense, the shared
     C14/C15 GND via.

   Then `gnd_fanout.py` adds GND vias next to every GND pad.
5. Autorouting in two stages:
   - `autoroute_stage.sh` routes the 19 nets that cross the area between the MCU
     and the display connector (panel QSPI, touch/I²C, debug, reset, macro, amp
     enable, CAL).
   - `maze.py --ripup` (a grid router with rip-up and history costs written for
     this board) finishes the ones Freerouting left open.
   - `autoroute_from.sh` then routes the remaining nets.
   - `maze.py` with `--layers F,B,In2 --layer-cost In2=6 --inner-keepout ...`
     joins the last few, using In2 only where F/B were full and never under the
     RF, crystal, DAC, amplifier or U5 areas.

   `ar_keepouts.py` adds temporary keep-outs over the hand-routed analog and RF
   areas for every autorouter run.
6. `pours.py`: F/B GND pours plus a stitching-via grid.
7. `cleanup.py` / `clean_loop.sh`: removes the escape vias the routers did not
   use (extending any off-centre track ends to the via centre first) and gives
   starved exposed pads a solid zone connection.
8. `maze.py --audio-aware --reroute`: re-routed PANEL_D0, PANEL_D1, PANEL_D3,
   USB_MCU_P/N and ENC_B. Fast nets keep ≥ 0.8 mm (same layer) / 0.55 mm
   (adjacent layer) from the analog nets, so the audio check passes. The RF/DAC
   keep-outs were active for this pass.
9. `finalize.sh BOARD REL --no-pour` writes the release:
   - DRC reports (json and rpt)
   - the audio crosstalk check (`audio_xtalk_check.py`)
   - the placement check
   - 3D models
   - fabrication files (`tooling/export_fab.sh`)
   - JLC CPL (`tooling/jlc_cpl.py`), BOMs (`build_bom.py`) and the orientation
     list (`orient_check.py`)

Post-review board edits are in [`postreview/`](postreview/README.md).

Helpers: `wdrc.py` (DRC summary through kicad-cli), `netlist_diff.py`
(`verification/netlist-diff-vs-r6.txt`), `final_checks.py`
(antenna-clearance distances, vias near the crystal, inner-layer tracks), `render_place.py` (PNG
renders for review), `unlock_nonskeleton.py` (lets the rip-up router move
autorouted copper but not the hand-routed skeleton).
