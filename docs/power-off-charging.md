# Power-off while charging (Rev B)

The Rev B Embassy firmware can finish an explicit power-off through a dim,
charging-only screen. Automatic power-on when USB is inserted into a deck
already in PMU shutdown is **not implemented**: the unmodified Rev B board
does not provide a reliable signal for that wake event.

## Current behavior

Hold CANCEL for the normal power-off gesture. The firmware releases the host
session and requests a fresh observation of the BQ24073's physical CHG pin
before choosing the next state.

| Physical charger observation | Result |
| --- | --- |
| CHG low: charging active | Show the dim charging-only page, with battery estimate and a PTT power-on hint. |
| CHG high: charging inactive, including an already-full battery | Complete ordinary PMU power-off. |
| No fresh observation within the entry timeout | Complete ordinary PMU power-off rather than assume charging. |

During charging-only mode:

- Bluetooth advertising and any existing Bluetooth connection stop. Microphone,
  speaker and normal deck controls stop; host deck and voice traffic are ignored.
- USB system RPC remains available, including requests to reboot normally or
  into firmware recovery. This is an intermediate shutdown state with firmware
  still running, not PMU shutdown.
- A new, deliberate two-second PTT hold reboots into normal operation. PTT held
  on entry must first be released; a short hold does not switch the deck on.
- CHG inactive for two seconds completes PMU shutdown and turns the display
  off. Charging becoming active again, or observations becoming unavailable,
  cancels that pending stop confirmation.

CHG inactive can mean completion, unplugging, a charging pause or a fault.
The firmware deliberately treats these as the same shutdown outcome. It does
not label every stop as a full battery or force the estimate to 100%. TI's
[BQ24073 datasheet, sections 7 and 9.3.5.7](https://www.ti.com/lit/ds/symlink/bq24073.pdf)
describes CHG as an open-drain charge-status output, rather than a dedicated
input-power detector. The battery percentage remains the approximate estimate
described in [battery-estimation.md](battery-estimation.md).

The mode polls physical CHG independently of normal voltage sampling, normally
once per second, and rejects stale observations. It does not use USB configured
or suspended state as proof that a power cable is present. A wall supply can
therefore enter this mode while CHG is active even without USB enumeration.
The normal ADC service deadline remains separate. No charger current,
termination, timer, protection or other charger register setting is changed.

Implementation: [shutdown entry and charging loop](../src/app.rs),
[fresh CHG observations](../src/battery/mod.rs),
[charging shutdown policy](../src/power.rs),
[link and voice shutdown](../src/link/mod.rs), and
[standalone charging page](../src/ui/charging.rs). This path is specific to the
Rev B Embassy build (`hw-b`); it does not establish equivalent behavior for
Rev A or the native firmware target.

## Why USB insertion cannot reliably wake the current board

Rev B routes CHG to PA33. Its charger PGOOD output and the MCU's dedicated
VBUS input are unconnected, as recorded in the board's
[main wiring](../hardware/rev-b/source/main/src/main.cohdl) and
[power wiring](../hardware/rev-b/source/main/src/power.cohdl).

The SF32LB52x supports shutdown/hibernate wake through selected always-on
pins, but PA28–PA33 are excluded. PA33 can be read by running firmware; arming
a PMU wake slot for it does not add the missing hardware capability. See
SiFli's [low-power development guide](https://docs.sifli.com/projects/sdk/latest/en/sf32lb52x/app_note/low_power.html)
and the applicable [SF32LB520/523/525/527 hardware guide, wake-source table](https://github.com/OpenSiFli/SiFli-Wiki/blob/main/source/en/hardware/SF32LB520-3-5-7-HW-Application.md).

D+ and D− reach PA35 and PA36, which are wake-capable pads. That alone does
not make them USB power detectors: with the USB PHY and pull-up disabled for
shutdown, insertion need not produce a usable data-line transition. A
charge-only cable or supply need not provide USB data activity at all. This
cannot guarantee automatic power-on for every charger, including when the
battery is already full. SiFli's [USB integration guidance](https://docs.sifli.com/projects/solution/2.application-notes/usb/index.html)
calls for an external VBUS detection connection to an IO for insertion/removal
detection.

The PMUC charger wake source belongs to the MCU's internal charger. It does
not connect the external BQ24073's CHG or PGOOD signal to the PMU, and the
board leaves MCU VBUS unconnected. The
[pinned SDK PMU implementation](https://github.com/OpenSiFli/SiFli-SDK/blob/006d03293e0621d694335cd4031773279fa2f19a/drivers/hal/bf0_hal_pmu.c)
exposes internal charger control; it is not evidence that connecting VBUS and
holding that charger disabled provides a validated wake path.

Ordinary PMU power-off therefore retains the existing PTT wake behavior.
Keeping firmware alive to poll for a cable would be a different power state
and would need its own power measurements; it would not satisfy automatic
wake from true PMU shutdown.

## Future hardware route and validation

A future board revision can route BQ24073 PGOOD to a valid, dedicated PMU
wake pad, with a pull-up to retained IO_3V3. PGOOD reports valid input power
independently of USB enumeration or charge completion. This covers a PC,
wall supply and already-full battery without enabling a second charger.
TI documents its open-drain behavior in the same datasheet cited above.

All currently usable wake pads have assignments. One possible reassignment
is to move touch RESET from PA44 to an unused ordinary GPIO such as PA32,
then use PA44 for PGOOD. This is a proposal requiring electrical/layout review,
not a modification made to the current board. Preserve PTT on wake slot 0;
use a falling edge on slot 1 for active-low PGOOD, and handle that wake cause
separately from the PTT hold gate. Edge triggering avoids immediate repeated
wake when charging finishes and the cable remains inserted. Validate input
thresholds, retained pulls, leakage and cable bounce on the revised hardware.

Connecting USB 5 V directly to a normal GPIO is not appropriate. Connecting
the MCU's dedicated VBUS input is also a power-path change involving its
internal charger and the shared battery node. Its safe disabled state and
VBUS-detection/wake operation through shutdown and boot would need specific
manufacturer confirmation and bench validation before choosing that route.

This document records implemented software behavior and source-based hardware
constraints. It does not claim a released image, completed flash, measured
shutdown current or physical acceptance of the new mode. On 2026-11-09,
`tools/test-all.sh` passed all 19 steps: 570 shared tests and 639 native-core
tests for each of LilyGo, Rev A and Rev B, all firmware/static-library builds,
execution-layout checks and the C panel/speaker/recovery checks. Charging-screen
previews were inspected in all four interface languages. The policy regressions
cover stop debounce, unavailable observations, queued PTT edges and timer wrap.
Physical acceptance must additionally check charging-only entry
on both a PC and wall supply, visible dim output, radio/audio shutdown,
unplug/charge-stop shutdown, deliberate PTT restart and already-full entry.
Automatic USB wake remains a separate hardware-dependent requirement.
