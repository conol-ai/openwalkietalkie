//! Display-unit battery monitoring and charger management.
//!
//! Sources: the SGM41562S datasheet and SiFli's
//! rtos/rtthread/bsp/sifli/drivers/drv_adc.c. VBAT is wired directly
//! to the display's cell; channel 7 has an internal nominal 2.01:1 divider.
//!
//! The dev board's SGM41562S is configured and polled over I2C. Rev A's
//! BQ24073 is a standalone charger with no bus: charging is automatic (CE is
//! strapped), its CHG output only reaches test pad TP1, so on `hw-a` the
//! charge state stays `Unknown` and the estimator runs voltage-only: it
//! follows the discharge curve in both directions at the usual 1%/minute,
//! never shows charging, and 100 means the top of the curve rather than a
//! confirmed termination. Rev B (`hw-b`) wires the same charger's CHG
//! output to PA33: low while the cell charges, high at termination, in a
//! fault and with no input, and the USB link says whether a host is there
//! (the chip's VBUS pin is not), so Rev B shows charging and confirms a
//! full charge the way the dev board does; on a charger-only supply the
//! end of the charge reads as no input and the estimate holds. The VBAT
//! sense is the same on every board.

#[cfg_attr(feature = "hw-a", allow(dead_code))]
mod charger;
pub mod state;
// Glob: shared `ui`/`deck` code sees the whole state.rs API as `crate::battery`,
// exactly as the consumers that mount state.rs directly.
pub use state::*;

use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
use embassy_time::{with_timeout, Duration, Instant, Timer};
#[cfg(feature = "hw-b")]
use sifli_hal::gpio::Input;
use sifli_hal::{
    adc::{Adc, Channel},
    mode::Async,
};

use crate::{board::BusDevice, logln};

pub static BATTERY: Signal<CriticalSectionRawMutex, BatteryState> = Signal::new();

// Rev B's physical CHG output remains useful without USB enumeration: a
// wall supply can charge the cell too. High means charging stopped (done,
// cable removed, or a charger fault), not proof of a particular cause.
#[cfg(feature = "hw-b")]
static CHARGER_ACTIVE: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);
#[cfg(feature = "hw-b")]
static CHARGER_READ_AT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
#[cfg(feature = "hw-b")]
static CHARGING_SCREEN: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
#[cfg(feature = "hw-b")]
static POLL_CHARGER: Signal<CriticalSectionRawMutex, ()> = Signal::new();
#[cfg(feature = "hw-b")]
const CHARGER_FRESH_MS: u32 = 1250;
// One cached inactive observation must expire before it can finish shutdown.
#[cfg(feature = "hw-b")]
const _: () = assert!(CHARGER_FRESH_MS < crate::power::CHARGING_STOP_MS);

/// Request fresh physical charger observations for a pending power-off.
/// The ordinary ADC/watchdog service deadline remains independent.
#[cfg(feature = "hw-b")]
pub fn prepare_power_off() {
    use core::sync::atomic::Ordering;
    CHARGER_ACTIVE.store(0, Ordering::Release);
    CHARGING_SCREEN.store(true, Ordering::Release);
    POLL_CHARGER.signal(());
}

/// A recent CHG-pin observation, never a cached USB configured/suspend state.
#[cfg(feature = "hw-b")]
pub fn charging_active() -> Option<bool> {
    use core::sync::atomic::Ordering;
    let value = CHARGER_ACTIVE.load(Ordering::Acquire);
    let at = CHARGER_READ_AT.load(Ordering::Acquire);
    let now = Instant::now().as_millis() as u32;
    if now.wrapping_sub(at) > CHARGER_FRESH_MS {
        return None;
    }
    match value {
        1 => Some(false),
        2 => Some(true),
        _ => None,
    }
}

fn charging_screen() -> bool {
    #[cfg(feature = "hw-b")]
    { CHARGING_SCREEN.load(core::sync::atomic::Ordering::Acquire) }
    #[cfg(not(feature = "hw-b"))]
    { false }
}

#[embassy_executor::task]
pub async fn task(
    adc: Adc<'static, Async>,
    mut channel: Channel<'static>,
    calibration: Option<state::Calibration>,
    mut bus: BusDevice,
    #[cfg(feature = "hw-b")] charge_status: Input<'static>,
) {
    #[cfg(not(feature = "hw-a"))]
    let charger_ready = match charger::initialize(&mut bus) {
        Ok(()) => {
            logln!("charger: SGM41562S configured: 344mA (350mA target), 4200mV, input 500mA, watchdog 64s");
            true
        }
        Err(error) => {
            logln!("charger: initialization failed: {:?}", error);
            false
        }
    };
    #[cfg(not(feature = "hw-a"))]
    {
        Timer::after_millis(100).await;
        log_charger_registers(&mut bus);
    }
    #[cfg(all(feature = "hw-a", not(feature = "hw-b")))]
    let charger_ready = {
        // Nothing answers at 0x03 on Rev A, and probing it would only add
        // traffic to the touch controller's bus.
        logln!("charger: Rev A BQ24073 is standalone (no I2C); charge state is not visible to firmware");
        let _ = &mut bus;
        false
    };
    #[cfg(feature = "hw-b")]
    let charger_ready = {
        // The same standalone charger, but its CHG output reaches PA33.
        logln!(
            "charger: Rev B BQ24073 is standalone (no I2C); CHG on PA33 reads {}",
            if charge_status.is_low() { "charging" } else { "not charging" }
        );
        let _ = &mut bus;
        false
    };
    // The pinned HAL enables slot 7 but reads RDATA0, without configuring
    // either slot's positive input. Route both to VBAT (channel 7), matching
    // HAL_ADC_ConfigChannel's slot-0 routing for a single conversion. Without
    // this, this board returns ~1330 counts from the wrong input.
    sifli_hal::pac::GPADC.slot(0).modify(|w| {
        w.set_pchnl_sel(7);
        w.set_slot_en(true);
    });
    sifli_hal::pac::GPADC.slot(7).modify(|w| w.set_pchnl_sel(7));
    let mut adc = Some(adc);
    #[cfg(any(not(feature = "hw-a"), feature = "hw-b"))]
    let mut estimator = state::Estimator::new();
    #[cfg(all(feature = "hw-a", not(feature = "hw-b")))]
    let mut estimator = state::Estimator::voltage_only();
    let mut previous = BatteryState::UNKNOWN;
    let mut previous_status = None;
    let mut service_at: Option<u32> = None;
    let mut status_at: Option<u32> = None;
    loop {
        let now = Instant::now().as_millis() as u32;
        let service_due =
            service_at.is_none_or(|at| now.wrapping_sub(at) >= crate::power::BATTERY_INTERVAL_MS);
        let fast_status = charging_screen() || previous.near_full() || estimator.completion_since().is_some();
        let status_due =
            service_due || fast_status && status_at.is_none_or(|at| now.wrapping_sub(at) >= 1000);
        if status_due {
            status_at = Some(now);
            // Extra samples on Done edges never reset the watchdog deadline.
            if service_due {
                service_at = Some(now);
                if charger_ready {
                    if let Err(error) = charger::feed_watchdog(&mut bus) {
                        logln!("charger: watchdog feed failed: {:?}", error);
                    }
                }
            }
            #[cfg(not(feature = "hw-b"))]
            let raw_status = if charger_ready {
                charger::read_reg(&mut bus, charger::REG_STATUS).ok()
            } else {
                None
            };
            // Rev B: the CHG pin, read against the last published voltage
            // (the sample below is fresh for the estimate itself).
            #[cfg(feature = "hw-b")]
            let raw_status = {
                use core::sync::atomic::Ordering;
                let charging = charge_status.is_low();
                CHARGER_READ_AT.store(now, Ordering::Release);
                CHARGER_ACTIVE.store(if charging { 2 } else { 1 }, Ordering::Release);
                Some(ChargeState::from_chg_pin(
                    charging,
                    crate::link::usb_up(),
                    previous.millivolts,
                    previous.full_mv,
                ).as_register())
            };
            let done_edge = estimator.observe_charger(raw_status, now);
            if service_due && charger_ready {
                if let (Some(status), Ok(faults)) =
                    (raw_status, charger::read_reg(&mut bus, charger::REG_FAULT))
                {
                    let faults = faults & 0x3f;
                    if previous_status != Some((status, faults)) {
                        logln!("charger: status=0x{:02X}, faults=0x{:02X}", status, faults);
                        previous_status = Some((status, faults));
                    }
                }
            }
            if service_due || done_edge {
                let mut state = BatteryState::UNKNOWN;
                if let Some(reader) = adc.as_mut() {
                    match with_timeout(
                        Duration::from_millis(100),
                        sample_mv(reader, &mut channel, calibration),
                    )
                    .await
                    {
                        Ok(Some(mv)) => state.millivolts = Some(mv),
                        Ok(None) => {}
                        Err(_) => {
                            // The HAL's read is not cancellation-safe. Drop the
                            // ADC and explicitly power down its VBAT monitor.
                            adc = None;
                            sifli_hal::pac::HPSYS_CFG
                                .anau_cr()
                                .modify(|w| w.set_en_vbat_mon(false));
                            logln!("battery: ADC timeout; voltage unavailable until reboot");
                        }
                    }
                }
                state.charge = raw_status
                    .map(ChargeState::from_register)
                    .unwrap_or(ChargeState::Unknown);
                if charger_ready {
                    if let Ok(value) = charger::read_reg(&mut bus, charger::REG_VOLTAGE) {
                        state.full_mv = charger::full_voltage_mv(value);
                    } else {
                        estimator.observe_charger(None, now);
                    }
                }
                // Only a real ADC sample advances filtering and completion.
                let state = estimator.update(state, now);
                if state != previous {
                    logln!(
                        "battery: {:?} mV, voltage ~{:?}%, reported ~{:?}%, {:?} (charge limit {} mV)",
                        state.millivolts,
                        state.voltage_percentage(),
                        state.percentage(),
                        state.charge,
                        state.full_mv
                    );
                    previous = state;
                }
                BATTERY.signal(state);
            }
        }
        let now = Instant::now().as_millis() as u32;
        let until_service = crate::power::BATTERY_INTERVAL_MS
            .saturating_sub(now.wrapping_sub(service_at.unwrap_or(now)));
        let until_status = if charging_screen() || previous.near_full() || estimator.completion_since().is_some() {
            1000u32.saturating_sub(now.wrapping_sub(status_at.unwrap_or(now)))
        } else {
            until_service
        };
        let wait = Timer::after_millis(until_service.min(until_status).max(1) as u64);
        #[cfg(feature = "hw-b")]
        embassy_futures::select::select(wait, POLL_CHARGER.wait()).await;
        #[cfg(not(feature = "hw-b"))]
        wait.await;
    }
}

#[cfg(not(feature = "hw-a"))]
fn log_charger_registers(bus: &mut BusDevice) {
    let mut registers = [0u8; 13];
    for (reg, value) in registers.iter_mut().enumerate() {
        match charger::read_reg(bus, reg as u8) {
            Ok(read) => *value = read,
            Err(error) => {
                logln!("charger: REG{:02X} read failed: {:?}", reg, error);
                return;
            }
        }
    }
    logln!("charger: REG00..0C {:02X?}", registers);
    logln!(
        "charger: enabled={}, USB={}, input suspended={}, status=0x{:02X}, faults=0x{:02X}",
        registers[1] & 0x08 == 0,
        registers[8] & 0x02 != 0,
        registers[1] & 0x10 != 0,
        registers[8],
        registers[9] & 0x3f,
    );
}

async fn sample_mv(
    adc: &mut Adc<'static, Async>,
    channel: &mut Channel<'static>,
    calibration: Option<state::Calibration>,
) -> Option<u16> {
    // Discard the first conversion, then average eight to reduce ADC noise.
    adc.read(channel).await.ok()?;
    let mut total = 0u32;
    let mut raw = 0u32;
    for _ in 0..8 {
        let sample = adc.read(channel).await.ok()?;
        total += sample.to_mv() as u32;
        raw += sample.value() as u32;
        Timer::after_millis(1).await;
    }
    let mv = match calibration {
        Some(cal) => cal.convert(((raw + 4) / 8) as u16),
        None => state::battery_mv(((total + 4) / 8) as u16),
    };
    if mv.is_none() {
        logln!(
            "battery: invalid ADC sample (raw {}, divider {} mV)",
            raw / 8,
            total / 8
        );
    }
    mv
}
