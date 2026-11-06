//! Dispatch product controls and deck host messages to the shared UI state,
//! and push its bands to the panel.
//!
//! Gating: deck events go out on channel 5 only while the deck host is
//! present and has spoken on this link; until then, while the demo data is
//! active, they are answered by the built-in host (`demo_loopback`). An
//! `Update` overflow ends the link after `up` (`link::overflow`). An
//! `Update` power-off (CANCEL held for `global::POWER_HOLD_MS`) ends the
//! session, darkens the panel and hands the chip to the PMU (`power_off`).
use crate::{
    battery::BATTERY,
    control::{now_ms, Stamped, CONTROLS},
    deck::msg::HostMsg,
    display::{Band, Co5300, BANDS, BAND_H},
    link::DECK_RX,
    logln,
    power::{self, Policy, Screen},
    ui::{self, damage::Damage, signature::Signature, UiState, Update},
};
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use embassy_futures::select::{select3, select4, Either3, Either4};
use embedded_graphics::{prelude::*, primitives::Rectangle};
use sifli_hal::lcdc::Error;

/// Set by `emit` when an update asks for the power-off; `run` acts on it
/// once its frame is settled.
static POWER_OFF: AtomicBool = AtomicBool::new(false);
/// Likewise for a confirmed factory reset (`Update::factory_reset`).
static FACTORY_RESET: AtomicBool = AtomicBool::new(false);
/// A host's `sys.reboot`: 1 = plain reboot, 2 = into the bootloader's USB
/// recovery mode. `run` acts on it after the reply has had time to leave.
static REBOOT: AtomicU8 = AtomicU8::new(0);

/// The host asked for a reboot (`link::wire::Handled::Reboot`).
pub fn request_reboot(recovery: bool) {
    REBOOT.store(if recovery { 2 } else { 1 }, Ordering::Release);
}

#[derive(Clone, Copy)]
enum Panel {
    Awake,
    Asleep(u32),
    Waking(u32),
}

/// The previous frame's signature: bands it still describes are not redrawn.
struct Frames {
    previous: Signature,
    next: Signature,
    valid: bool,
}

/// Bench `panel-debug`: a host request for the panel wakes the loop.
async fn panel_diag_wake() {
    #[cfg(feature = "panel-debug")]
    crate::display::diag::WAKE.wait().await;
    #[cfg(not(feature = "panel-debug"))]
    core::future::pending::<()>().await;
}

/// The session table: 255 sessions in PSRAM on Rev A and Rev B (4.7 KB
/// each, 1.2 MB of the 7 MB below the diagnostic scratch area), the old 8 in
/// SRAM on the LilyGo dev board. PSRAM is NOLOAD, so the table is written
/// in place, one session at a time, before anything reads it. A PSRAM that
/// does not read back what was written falls back to the SRAM table rather
/// than running on garbage.
fn attach_sessions(state: &mut UiState) {
    use crate::deck::model::Agent;
    #[cfg(feature = "hw-a")]
    use crate::deck::model::DEFAULT_SESSIONS;
    const SESSIONS: usize = crate::ui::state::SESSIONS;
    #[cfg_attr(feature = "hw-a", unsafe(link_section = ".psram_bss"))]
    static mut TABLE: core::mem::MaybeUninit<[Agent; SESSIONS]> = core::mem::MaybeUninit::uninit();
    #[cfg(feature = "hw-a")]
    static mut FALLBACK: core::mem::MaybeUninit<[Agent; DEFAULT_SESSIONS]> = core::mem::MaybeUninit::uninit();
    // SAFETY: called once, before the UI runs; whichever table is chosen is
    // reached only through `state.deck.agents` from here on.
    unsafe {
        let table = core::ptr::addr_of_mut!(TABLE).cast::<Agent>();
        #[cfg(feature = "hw-a")]
        let (base, len) = if psram_answers(table.cast(), core::mem::size_of::<[Agent; SESSIONS]>()) {
            (table, SESSIONS)
        } else {
            crate::logln!("sessions: PSRAM does not read back; holding {} in SRAM", DEFAULT_SESSIONS);
            (core::ptr::addr_of_mut!(FALLBACK).cast::<Agent>(), DEFAULT_SESSIONS)
        };
        #[cfg(not(feature = "hw-a"))]
        let (base, len) = (table, SESSIONS);
        for i in 0..len {
            base.add(i).write(Agent::new());
        }
        state.deck.agents.attach(base, len);
        crate::logln!("sessions: {} held", len);
    }
}

/// The first and last words of `len` bytes at `at` keep two patterns.
#[cfg(feature = "hw-a")]
unsafe fn psram_answers(at: *mut u32, len: usize) -> bool {
    let words = len / 4;
    let probes = [at, at.add(words - 1)];
    [0xA5A5_5A5Au32, 0x0F1E_2D3C].iter().all(|&pattern| {
        probes.iter().all(|&p| {
            core::ptr::write_volatile(p, pattern);
            core::ptr::read_volatile(p) == pattern
        })
    })
}

pub async fn run(mut display: Option<Co5300>, band: &'static mut Band) -> ! {
    // Several kilobytes: const-initialised in its static (a `StaticCell` would
    // build it on this task's stack first and copy) and only filled in place.
    static STATE: static_cell::ConstStaticCell<UiState> =
        static_cell::ConstStaticCell::new(UiState::new());
    static SCRATCH: static_cell::ConstStaticCell<HostMsg> =
        static_cell::ConstStaticCell::new(HostMsg::None);
    static FRAMES: static_cell::ConstStaticCell<Frames> =
        static_cell::ConstStaticCell::new(Frames {
            previous: Signature::new(),
            next: Signature::new(),
            valid: false,
        });
    let state = STATE.take();
    let scratch = SCRATCH.take();
    let frames = FRAMES.take();
    attach_sessions(state);
    state.reset();
    // A deck that never finished the tutorial starts in it (Settings >
    // Display > Tutorial replays it later).
    if !crate::link::prefs().tutorial_done {
        state.start_tutorial(false);
        logln!("app: first run: starting the tutorial");
    }
    let mut power = Policy::new(now_ms());
    let mut panel = Panel::Awake;
    let mut shown = Screen::Active;
    let mut applied = brightness(&power, state);
    let mut dirty = false;
    logln!("app: Agent Command Deck ready");
    redraw(&mut display, band, state, frames, true).await;
    loop {
        let timeout = if !matches!(panel, Panel::Awake) && power.screen() != Screen::Off {
            crate::power::ACTIVE_POLL_MS
        } else if power.screen() == Screen::Off {
            // Nothing animates on a dark panel; timers are read lazily on wake.
            power.poll_ms()
        } else {
            power.screen().animation_ms()
                .min(ui::state::arcade::poll_ms(state, now_ms()))
                .min(if power.held() { power.poll_ms() } else { 500 })
        };
        dirty |= match select4(
            CONTROLS.receive(),
            BATTERY.wait(),
            select3(
                crate::link::UI_STATUS.wait(),
                crate::link::UI_BLUETOOTH.wait(),
                embassy_futures::select::select(
                    embassy_time::Timer::after_millis(timeout as u64),
                    panel_diag_wake(),
                ),
            ),
            // A parsed `HostMsg` is over half a kilobyte (one `cardtext`
            // window). The select waits for one to be there and it is taken
            // afterwards: carried through the select's own output it would
            // cross this task's stack three times over.
            DECK_RX.ready_to_receive(),
        )
        .await
        {
            Either4::First(stamped) => apply(state, &mut power, scratch, stamped),
            Either4::Second(battery) => {
                crate::link::battery(battery);
                state.update_battery(battery)
            }
            Either4::Third(Either3::First(connected)) => {
                let mut update = Update::new();
                if state.connected != connected {
                    state.set_connected(connected, &mut update);
                }
                emit(state, &mut power, scratch, &mut update)
            }
            Either4::Third(Either3::Second(bluetooth)) => state.update_bluetooth(bluetooth),
            Either4::Third(Either3::Third(_)) => false,
            Either4::Fourth(()) => host(state, &mut power, scratch),
        };
        #[cfg(feature = "panel-debug")]
        if let Some(op) = crate::display::diag::REQUEST.try_take() {
            let reply = crate::display::diag::run(display.as_mut(), op).await;
            crate::display::diag::REPLY.signal(reply);
            dirty = true;
        }
        dirty |= tick(state, &mut power, scratch);
        while let Ok(stamped) = CONTROLS.try_receive() {
            dirty |= apply(state, &mut power, scratch, stamped);
        }
        let now = now_ms();
        let desired = power.tick(now);
        dirty |= state.set_screen_lock(power.lock_progress());
        if POWER_OFF.swap(false, Ordering::AcqRel) {
            #[cfg(feature = "hw-a")]
            power_off(display.take(), state, &mut power, scratch).await;
            #[cfg(not(feature = "hw-a"))]
            logln!("power: off requested; this board has no soft power-off");
        }
        if FACTORY_RESET.swap(false, Ordering::AcqRel) {
            #[cfg(feature = "hw-a")]
            factory_reset(display.take(), state, &mut power, scratch).await;
            #[cfg(not(feature = "hw-a"))]
            {
                // No reboot path on the dev board: erase, and start over in
                // place as a fresh deck would.
                crate::link::erase_storage();
                state.reset();
                state.start_tutorial(false);
                dirty = true;
                logln!("app: factory reset: storage erased, UI reset; this board does not restart");
            }
        }
        match REBOOT.swap(0, Ordering::AcqRel) {
            0 => {}
            #[cfg(feature = "hw-a")]
            mode => reboot(display.take(), mode == 2).await,
            #[cfg(not(feature = "hw-a"))]
            _ => logln!("power: reboot requested; this board has no software reboot path"),
        }
        if let Some(driver) = display.as_mut() {
            let result = match panel {
                Panel::Awake if desired == Screen::Off => driver.sleep().map(|()| {
                    power.slept();
                    panel = Panel::Asleep(now);
                    shown = Screen::Off;
                    dirty = true;
                    logln!("power: screen off");
                }),
                Panel::Asleep(at) if desired != Screen::Off && now.wrapping_sub(at) >= 120 => {
                    driver.wake().map(|()| {
                        panel = Panel::Waking(now);
                    })
                }
                // Dim/active transitions and the Settings brightness rows.
                Panel::Awake if brightness(&power, state) != applied => {
                    let level = brightness(&power, state);
                    driver.set_brightness(level).map(|()| {
                        applied = level;
                        if desired != shown {
                            shown = desired;
                            logln!(
                                "power: screen {}",
                                if desired == Screen::Dim {
                                    "dim"
                                } else {
                                    "active"
                                }
                            );
                        }
                    })
                }
                _ => Ok(()),
            };
            if let Err(error) = result {
                logln!("power: panel transition failed: {:?}", error);
            }
        }
        let waking = matches!(panel, Panel::Waking(at) if now.wrapping_sub(at) >= 120);
        if desired != Screen::Off && (waking || (matches!(panel, Panel::Awake) && dirty)) {
            if redraw(&mut display, band, state, frames, waking).await {
                dirty = false;
                if waking {
                    let level = brightness(&power, state);
                    if let Some(driver) = display.as_mut() {
                        if let Err(error) = driver
                            .set_brightness(level)
                            .and_then(|()| driver.enable())
                        {
                            logln!("power: display-on failed: {:?}", error);
                            continue;
                        }
                    }
                    applied = level;
                    panel = Panel::Awake;
                    shown = desired;
                    // Controls queued during the render are judged by their
                    // stamps: they were pressed in the dark.
                    let lit = now_ms();
                    power.frame_ready(lit);
                    state.frame_shown(lit);
                    logln!("power: screen awake after full refresh");
                }
            }
        }
    }
}

fn brightness(power: &Policy, state: &UiState) -> u8 {
    power::brightness(power.screen(), state.user_brightness())
}

/// Act on one update; true when a redraw is due.
fn emit(state: &mut UiState, power: &mut Policy, scratch: &mut HostMsg, update: &mut Update) -> bool {
    if update.wake {
        power.notify(now_ms());
    }
    let online = state.deck.host_present && crate::link::deck_spoke();
    if !online && state.deck.demo_active() {
        // The built-in host's answers are applied into this same update.
        state.demo_loopback(now_ms(), update, scratch);
    }
    for event in update.deck_events() {
        // A PTT press opens the microphone alongside its `ptt` line; its
        // release closes it even when the host has gone away meanwhile.
        crate::audio::voice::on_deck_event(&event, online);
        if online {
            crate::link::deck_event(event, state);
        }
    }
    if update.overflow {
        crate::link::overflow();
    }
    if update.power_off {
        POWER_OFF.store(true, Ordering::Release);
    }
    if update.factory_reset {
        FACTORY_RESET.store(true, Ordering::Release);
    }
    if update.tutorial_done {
        // A few milliseconds with interrupts off: one journal slot.
        crate::link::save_prefs(crate::link::Prefs { tutorial_done: true });
    }
    crate::audio::arcade::publish(crate::ui::state::arcade::sound_command(state));
    if crate::ui::state::arcade::playing(state) && power.screen() != Screen::Off {
        power.notify(now_ms());
    }
    update.dirty
}

/// Settings > About > Factory reset, confirmed: release the hosts as a
/// power-off does, blank the journal (pairing and preferences), darken the
/// panel and restart through the PMU. The deck comes back as new: no
/// pairing, and the tutorial first. Never returns.
#[cfg(feature = "hw-a")]
async fn factory_reset(
    mut display: Option<Co5300>,
    state: &mut UiState,
    power: &mut Policy,
    scratch: &mut HostMsg,
) -> ! {
    use embassy_time::Timer;
    logln!("app: factory reset confirmed; releasing the hosts");
    let mut update = Update::new();
    state.power_down(&mut update);
    emit(state, power, scratch, &mut update);
    // The last lines cross the link meanwhile (a BLE write can take 0.5 s).
    Timer::after_millis(500).await;
    if let Some(driver) = display.as_mut() {
        let _ = driver.sleep();
        Timer::after_millis(120).await;
        driver.power_down().await;
    }
    // Up to two seconds with interrupts off; the panel is already dark.
    if !crate::link::erase_storage() {
        logln!("app: factory reset: the erase failed; restarting anyway");
    }
    Timer::after_millis(20).await;
    crate::pmu::reboot(false)
}

/// A host asked for a reboot: let its reply leave, darken the panel, and go
/// through the PMU, into the bootloader's USB DFU recovery when `recovery`.
#[cfg(feature = "hw-a")]
async fn reboot(mut display: Option<Co5300>, recovery: bool) -> ! {
    use embassy_time::Timer;
    logln!(
        "power: rebooting into {}",
        if recovery { "USB recovery" } else { "the application" }
    );
    // The RPC reply crosses the link meanwhile.
    Timer::after_millis(400).await;
    if let Some(driver) = display.as_mut() {
        let _ = driver.sleep();
        Timer::after_millis(120).await;
        driver.power_down().await;
    }
    Timer::after_millis(20).await;
    crate::pmu::reboot(recovery)
}

/// A PMU wake is only a power-on if PTT stays down until
/// `pmu::POWER_ON_HOLD_MS` after the application started. A brush against
/// the button boots the chip, finds PTT up again and goes straight back to
/// sleep, the panel never having been lit (it is initialised but still
/// blanked here). A release is three consecutive 10 ms samples high, so one
/// noisy sample cannot end a real hold. Cold starts and reboots are never
/// gated: there is no button behind them.
#[cfg(feature = "hw-a")]
pub async fn power_on_gate(
    boot: crate::pmu::Boot,
    ptt: &sifli_hal::gpio::Input<'static>,
    display: &mut Option<Co5300>,
) {
    use embassy_time::{Instant, Timer};
    if !matches!(boot, crate::pmu::Boot::Wake { .. }) {
        return;
    }
    let deadline = Instant::from_millis(crate::pmu::POWER_ON_HOLD_MS as u64);
    let mut up_samples = 0;
    while Instant::now() < deadline {
        up_samples = if ptt.is_high() { up_samples + 1 } else { 0 };
        if up_samples >= 3 {
            logln!(
                "power: PTT released {} ms after start, before the {} ms power-on hold; back to sleep",
                Instant::now().as_millis(),
                crate::pmu::POWER_ON_HOLD_MS
            );
            if let Some(driver) = display.as_mut() {
                driver.power_down().await;
            }
            // Let that line leave the UART before the clocks stop.
            Timer::after_millis(20).await;
            crate::pmu::power_off()
        }
        Timer::after_millis(10).await;
    }
    logln!(
        "power: PTT held through the {} ms power-on hold; switching on",
        crate::pmu::POWER_ON_HOLD_MS
    );
}

/// The hold ran its course: release the hosts, darken the panel and cut its
/// rails, wait for a held PTT to come back up, then hand the chip to the
/// PMU. Its own frame, so `run`'s stays under the stack gate.
#[cfg(feature = "hw-a")]
async fn power_off(
    mut display: Option<Co5300>,
    state: &mut UiState,
    power: &mut Policy,
    scratch: &mut HostMsg,
) -> ! {
    use crate::control::{Button, Control, Edge};
    use embassy_time::{with_deadline, Duration, Instant, Timer};
    logln!(
        "power: off (CANCEL held {} ms); releasing the hosts",
        ui::state::global::POWER_HOLD_MS
    );
    let mut update = Update::new();
    state.power_down(&mut update);
    emit(state, power, scratch, &mut update);
    // The last lines cross the link meanwhile: about a millisecond over USB,
    // up to half a second over BLE.
    Timer::after_millis(500).await;
    if let Some(driver) = display.as_mut() {
        if let Err(error) = driver.sleep() {
            logln!("power: panel sleep failed: {:?}", error);
        }
        Timer::after_millis(120).await;
        driver.power_down().await;
        logln!("power: panel rails off");
    }
    // The wake detector wants a fresh falling edge of PTT: if it is down
    // (a hand on both buttons), wait for it to come up (capped, in case its
    // release never arrives). The CANCEL of the hold no longer matters: it
    // is not a wake pin.
    if power.is_down(Button::PushToTalk) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match with_deadline(deadline, CONTROLS.receive()).await {
                Ok((Control::Button(Button::PushToTalk, Edge::Release), _)) | Err(_) => break,
                Ok(_) => {}
            }
        }
    }
    logln!(
        "power: entering {}; {} powers the deck on",
        crate::pmu::mode(),
        crate::pmu::WAKE_BUTTONS
    );
    // Let that line leave the UART before the clocks stop.
    Timer::after_millis(20).await;
    crate::pmu::power_off()
}
fn tick(state: &mut UiState, power: &mut Policy, scratch: &mut HostMsg) -> bool {
    let now = now_ms();
    let mut update = Update::new();
    state.tick(now, power.idle_ms(now), power.screen() != Screen::Off, &mut update);
    emit(state, power, scratch, &mut update)
}
fn apply(
    state: &mut UiState,
    power: &mut Policy,
    scratch: &mut HostMsg,
    (control, at_ms): Stamped,
) -> bool {
    let before = power.screen();
    let deliver = power.input(control, at_ms);
    let lock_dirty = state.set_screen_lock(power.lock_progress());
    let dirty = tick(state, power, scratch) | (before != power.screen()) | lock_dirty;
    if !deliver {
        return dirty;
    }
    let mut update = Update::new();
    state.apply(control, at_ms, &mut update);
    dirty | emit(state, power, scratch, &mut update)
}
/// One channel-5 line from the GATT task, or why it could not be parsed.
/// Keep the received HostMsg and Update out of the async dispatch frame.
#[inline(never)]
fn host(state: &mut UiState, power: &mut Policy, scratch: &mut HostMsg) -> bool {
    // Only this task receives, so the line signalled ready is still there.
    let Ok(inbound) = DECK_RX.try_receive() else {
        return false;
    };
    let mut update = Update::new();
    match &inbound {
        Ok(msg) => state.host(msg, now_ms(), &mut update),
        Err(why) => state.reject(*why, now_ms(), &mut update),
    }
    emit(state, power, scratch, &mut update)
}
async fn redraw(
    display: &mut Option<Co5300>,
    band: &mut Band,
    state: &UiState,
    frames: &mut Frames,
    force: bool,
) -> bool {
    let Some(panel) = display.as_mut() else {
        return true;
    };
    if let Err(e) = render(panel, band, state, frames, force).await {
        logln!("app: render failed: {:?}", e);
        return false;
    }
    true
}

pub async fn render_splash(display: &mut Co5300, band: &mut Band) -> Result<(), Error> {
    for index in 0..BANDS {
        band.set_row(panel_band(index));
        ui::splash::draw(band);
        display.write_band(band).await?;
    }
    Ok(())
}

/// Draw and push the rectangles whose pixels changed since the last successful
/// render (all of them when `force`d or after a failure), one at a time.
async fn render(
    display: &mut Co5300,
    band: &mut Band,
    state: &UiState,
    frames: &mut Frames,
    force: bool,
) -> Result<(), Error> {
    ui::scene::signature(state, &mut frames.next);
    let previous = (!force && frames.valid).then_some(&frames.previous);
    let mut damage = Damage::between(previous, &frames.next);
    // A failed transfer invalidates the retained frame, so the next render
    // repairs the entire screen, including any partially transmitted region.
    frames.valid = false;
    while let Some(region) = damage.next() {
        band.set_row(region.y0 / BAND_H);
        band.clip_to(Rectangle::new(
            Point::new(region.x0 as i32, region.y0 as i32),
            Size::new((region.x1 - region.x0) as u32, (region.y1 - region.y0) as u32),
        ));
        ui::scene::draw(band, state);
        display.write_band(band).await?;
    }
    frames.next.copy_into(&mut frames.previous);
    frames.valid = true;
    Ok(())
}

// Render logical bands bottom first so the rotated writes still visit the
// panel's native rows from top to bottom.
fn panel_band(index: u16) -> u16 {
    if crate::orientation::ROTATE_180 {
        BANDS - 1 - index
    } else {
        index
    }
}
