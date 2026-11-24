//! Local-input inactivity policy, shared by both firmware runtimes.
//!
//! Sleeping locks every local control, including PTT. Any input lights the
//! screen; four counterclockwise wheel detents unlock it without scrolling
//! the underlying face. Clockwise rotation or another sleep resets progress.
//! A press that began locked is consumed through release, even after unlock.
//! The hidden wake-frame gate still covers controls queued during rendering. A
//! notice may light the screen without counting as input (`notify`), so
//! idle-home keeps the single idle clock (`idle_ms`).
use crate::control::{Button, Control, Edge, TouchPhase, Wheel};

pub const DIM_AFTER_MS: u32 = 30_000;
pub const OFF_AFTER_MS: u32 = 120_000;
pub use crate::control::UNLOCK_TICKS;
/// A notice keeps the screen lit this long unless real input arrives.
pub const NOTIFY_MS: u32 = 15_000;
pub const BATTERY_INTERVAL_MS: u32 = 15_000;
pub const IDLE_POLL_MS: u32 = 50;
/// Native IRQ latches wake immediately; this is dark-screen recovery only.
pub const DARK_POLL_MS: u32 = 500;
pub const ACTIVE_POLL_MS: u32 = 5;
/// A control stamped up to this long before the wake frame reached the glass
/// was made in the dark, even if the runtime dequeues it afterwards.
const QUEUED_MS: u32 = 1000;
pub const FULL_BRIGHTNESS: u8 = 255;
pub const DIM_BRIGHTNESS: u8 = 32;
/// Panel level for each user brightness setting 1..=5.
const LEVELS: [u8; 5] = [40, 80, 130, 190, FULL_BRIGHTNESS];

/// Confirm that charging stopped before completing an explicit shutdown.
pub const CHARGING_STOP_MS: u32 = 2_000;
/// A fresh deliberate PTT hold leaves the charging-only screen.
pub const CHARGING_POWER_ON_HOLD_MS: u32 = 2_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChargingDecision {
    Stay,
    RunNormally,
    PowerOff,
}

/// Charging-only mode after an explicit power-off while charging is active.
///
/// The caller supplies fresh charger evidence, not USB configuration state:
/// `Some(true)` is actively charging, `Some(false)` is stopped, and `None`
/// is unknown. Stopped may mean full, unplugged, paused or faulted; this
/// policy never infers which, nor does it infer cable presence or battery SOC.
pub struct ChargingShutdown {
    stopped_since: Option<u32>,
    ptt_since: Option<u32>,
    ptt_armed: bool,
    decision: ChargingDecision,
}

impl ChargingShutdown {
    pub const fn new(ptt_down_on_entry: bool) -> Self {
        Self {
            stopped_since: None,
            ptt_since: None,
            ptt_armed: !ptt_down_on_entry,
            decision: ChargingDecision::Stay,
        }
    }

    /// Consume a PTT edge at its original wrapping millisecond stamp, before
    /// polling with the current time. Process queued edges in order: a short
    /// release cancels that hold even when it is dequeued after its deadline.
    /// A release after a complete fresh hold can itself finish the hold.
    /// Charger confirmation is unaffected, and terminal decisions stay latched.
    pub fn observe_ptt(&mut self, at_ms: u32, ptt_down: bool) -> ChargingDecision {
        if self.decision != ChargingDecision::Stay {
            return self.decision;
        }
        if !ptt_down {
            if self.ptt_since.is_some_and(|since| {
                at_ms.wrapping_sub(since) >= CHARGING_POWER_ON_HOLD_MS
            }) {
                self.decision = ChargingDecision::RunNormally;
            }
            self.ptt_armed = true;
            self.ptt_since = None;
        } else if self.ptt_armed {
            let since = *self.ptt_since.get_or_insert(at_ms);
            if at_ms.wrapping_sub(since) >= CHARGING_POWER_ON_HOLD_MS {
                self.decision = ChargingDecision::RunNormally;
            }
        }
        self.decision
    }

    /// Poll with monotonic milliseconds (wrapping u32) and current inputs.
    /// Drain stamped PTT edges through `observe_ptt` first. Unknown/active
    /// charging cancels stop confirmation. A PTT held on entry is ignored
    /// until released; subsequent short holds restart the hold clock. A
    /// completed hold wins a simultaneous stop deadline, and either terminal
    /// decision remains latched until this policy is dropped.
    pub fn update(&mut self, now_ms: u32, charging: Option<bool>, ptt_down: bool) -> ChargingDecision {
        if self.observe_ptt(now_ms, ptt_down) != ChargingDecision::Stay {
            return self.decision;
        }
        match charging {
            Some(false) => {
                let since = *self.stopped_since.get_or_insert(now_ms);
                if now_ms.wrapping_sub(since) >= CHARGING_STOP_MS {
                    self.decision = ChargingDecision::PowerOff;
                }
            }
            Some(true) | None => self.stopped_since = None,
        }
        self.decision
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Screen {
    Active,
    Dim,
    Off,
}
impl Screen {
    pub fn animation_ms(self) -> u32 {
        match self {
            Self::Active => 80,
            Self::Dim => 200,
            Self::Off => u32::MAX,
        }
    }
}

/// Panel brightness for `screen` at the user's level (1..=5). Dim never
/// exceeds the user's lit level.
pub fn brightness(screen: Screen, level: u8) -> u8 {
    let lit = LEVELS[(level.clamp(1, 5) - 1) as usize];
    match screen {
        Screen::Active => lit,
        Screen::Dim => DIM_BRIGHTNESS.min(lit),
        Screen::Off => 0,
    }
}

pub struct Policy {
    screen: Screen,
    /// Completed counterclockwise detents, or None while unlocked.
    lock_progress: Option<u8>,
    last_input: u32,
    buttons: u8,
    contact: bool,
    wake_contact: bool,
    /// Buttons pressed while blocked: their release is swallowed too.
    wake_buttons: u8,
    awaiting_frame: bool,
    /// When the last wake frame was shown, while controls stamped before it
    /// may still be queued.
    shown_at: Option<u32>,
    /// A control queued before the unlocking detent still belongs to the lock.
    unlocked_at: Option<u32>,
    notified: Option<u32>,
}
impl Policy {
    pub const fn new(now: u32) -> Self {
        Self {
            screen: Screen::Active,
            lock_progress: None,
            last_input: now,
            buttons: 0,
            contact: false,
            wake_contact: false,
            wake_buttons: 0,
            awaiting_frame: false,
            shown_at: None,
            unlocked_at: None,
            notified: None,
        }
    }
    pub fn screen(&self) -> Screen {
        self.screen
    }
    pub fn lock_progress(&self) -> Option<u8> {
        self.lock_progress
    }
    pub fn held(&self) -> bool {
        self.buttons != 0 || self.contact
    }
    /// Whether `button` is down, as far as this policy has seen.
    #[cfg_attr(not(feature = "hw-a"), allow(dead_code))]
    pub fn is_down(&self, button: Button) -> bool {
        self.buttons & button.bit() != 0
    }
    /// The UI's single idle clock: 0 while an unlocked control is held.
    pub fn idle_ms(&self, now: u32) -> u32 {
        if self.held() && self.lock_progress.is_none() {
            0
        } else {
            now.wrapping_sub(self.last_input)
        }
    }
    /// The panel really went to sleep (not merely the clock reaching Off:
    /// a busy panel may never sleep): its wake needs a full frame first.
    pub fn slept(&mut self) {
        self.awaiting_frame = true;
        self.lock_progress = Some(0);
    }
    /// A complete wake frame has been on the glass since `at`.
    pub fn frame_ready(&mut self, at: u32) {
        if core::mem::take(&mut self.awaiting_frame) {
            self.shown_at = Some(at);
        }
    }
    /// Local wake evidence without a complete key/touch command. Keep the
    /// hidden-frame gate intact so waking never clicks an invisible control.
    #[allow(dead_code)] // Raw wake evidence is supplied by the native runtime.
    pub fn wake(&mut self, now: u32) {
        self.activity(now);
    }
    /// A new notice: light the screen for `NOTIFY_MS` without counting as
    /// input, so idle-home and the dim/off clock are untouched.
    pub fn notify(&mut self, now: u32) {
        self.notified = Some(now);
        self.screen = Screen::Active;
    }
    pub fn poll_ms(&self) -> u32 {
        if self.screen == Screen::Off {
            DARK_POLL_MS
        } else if self.held() {
            ACTIVE_POLL_MS
        } else {
            IDLE_POLL_MS
        }
    }
    pub fn tick(&mut self, now: u32) -> Screen {
        let before = self.screen;
        // A button trapped in a bag must not keep the locked panel lit.
        if self.held() && self.lock_progress.is_none() {
            self.activity(now);
        } else if self
            .notified
            .is_some_and(|at| now.wrapping_sub(at) < NOTIFY_MS)
        {
            self.screen = Screen::Active;
        } else {
            self.notified = None;
            self.screen = match now.wrapping_sub(self.last_input) {
                idle if idle >= OFF_AFTER_MS => Screen::Off,
                idle if idle >= DIM_AFTER_MS => Screen::Dim,
                _ => Screen::Active,
            };
        }
        if self.screen == Screen::Off && before != Screen::Off {
            self.lock_progress = Some(0);
        }
        if self
            .shown_at
            .is_some_and(|at| now.wrapping_sub(at) >= QUEUED_MS)
        {
            self.shown_at = None;
        }
        if self
            .unlocked_at
            .is_some_and(|at| now.wrapping_sub(at) >= QUEUED_MS)
        {
            self.unlocked_at = None;
        }
        self.screen
    }
    /// True when the UI should see this control. Locked and dark-frame
    /// controls are consumed through release/cancel. Unlock detents never
    /// reach the underlying UI, including the fourth one.
    /// Host configuration/status traffic never calls this function.
    pub fn input(&mut self, input: Control, now: u32) -> bool {
        let unseen = self
            .shown_at
            .is_some_and(|at| at.wrapping_sub(now).wrapping_sub(1) < QUEUED_MS);
        let dark = self.screen == Screen::Off || self.awaiting_frame || unseen;
        let before_unlock = self
            .unlocked_at
            .is_some_and(|at| at.wrapping_sub(now).wrapping_sub(1) < QUEUED_MS);
        let blocked = dark || self.lock_progress.is_some() || before_unlock;
        match input {
            Control::Touch(touch) => {
                match touch.phase {
                    TouchPhase::Down => {
                        self.wake_contact = blocked || (self.contact && self.wake_contact);
                        self.contact = true;
                        self.activity(now);
                    }
                    TouchPhase::Move => {
                        if self.contact {
                            self.activity(now);
                        }
                    }
                    TouchPhase::Up | TouchPhase::Cancel => {
                        if self.contact {
                            self.activity(now);
                        }
                        self.contact = false;
                        return !core::mem::replace(&mut self.wake_contact, false) && !blocked;
                    }
                }
                !self.wake_contact && !blocked
            }
            Control::Button(button, edge) => {
                let mask = button.bit();
                if edge == Edge::Press {
                    let suppressed_hold = self.buttons & self.wake_buttons & mask != 0;
                    self.buttons |= mask;
                    self.activity(now);
                    if blocked || suppressed_hold {
                        self.wake_buttons |= mask;
                        return false;
                    }
                    self.wake_buttons &= !mask;
                    true
                } else {
                    // Repeated synthetic releases on a failed bus do not wake.
                    if self.buttons & mask != 0 {
                        self.activity(now);
                    }
                    self.buttons &= !mask;
                    let swallowed = self.wake_buttons & mask != 0;
                    self.wake_buttons &= !mask;
                    !swallowed && !blocked
                }
            }
            Control::Wheel(wheel) => {
                self.activity(now);
                if let Some(progress) = self.lock_progress {
                    self.lock_progress = match wheel {
                        Wheel::Ccw if progress + 1 >= UNLOCK_TICKS => {
                            self.unlocked_at = Some(now);
                            None
                        }
                        Wheel::Ccw => Some(progress + 1),
                        Wheel::Cw => Some(0),
                    };
                    return false;
                }
                !blocked
            }
        }
    }
    fn activity(&mut self, now: u32) {
        self.last_input = now;
        self.screen = Screen::Active;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::Touch;

    fn touch(phase: TouchPhase) -> Control {
        Control::Touch(Touch {
            phase,
            x: 200,
            y: 300,
        })
    }
    fn press(button: Button) -> Control {
        Control::Button(button, Edge::Press)
    }
    fn release(button: Button) -> Control {
        Control::Button(button, Edge::Release)
    }
    fn dark() -> Policy {
        let mut p = Policy::new(0);
        assert_eq!(p.tick(OFF_AFTER_MS), Screen::Off);
        p.slept();
        p
    }
    fn unlock(p: &mut Policy, at: u32) {
        for tick in 0..UNLOCK_TICKS {
            assert!(!p.input(Control::Wheel(Wheel::Ccw), at.wrapping_add(tick as u32)));
        }
        assert_eq!(p.lock_progress(), None);
    }

    #[test]
    fn charging_shutdown_waits_through_active_or_unknown_charging() {
        let mut policy = ChargingShutdown::new(false);
        for now in [0, 2_000, 60_000, 3_600_000] {
            for charging in [Some(true), None] {
                assert_eq!(policy.update(now, charging, false), ChargingDecision::Stay);
            }
        }
        assert_eq!(policy.update(3_600_001, Some(false), false), ChargingDecision::Stay);
        assert_eq!(policy.update(3_602_000, Some(false), false), ChargingDecision::Stay);
        assert_eq!(policy.update(3_602_001, Some(false), false), ChargingDecision::PowerOff);
    }

    #[test]
    fn charging_shutdown_requires_two_continuous_seconds_of_stopped_evidence() {
        for interruption in [Some(true), None] {
            let mut policy = ChargingShutdown::new(false);
            assert_eq!(policy.update(100, Some(false), false), ChargingDecision::Stay);
            assert_eq!(policy.update(2_099, Some(false), false), ChargingDecision::Stay);
            assert_eq!(policy.update(2_100, interruption, false), ChargingDecision::Stay);
            assert_eq!(policy.update(2_101, Some(false), false), ChargingDecision::Stay);
            assert_eq!(policy.update(4_100, Some(false), false), ChargingDecision::Stay);
            assert_eq!(policy.update(4_101, Some(false), false), ChargingDecision::PowerOff);
        }
    }

    #[test]
    fn charging_shutdown_ignores_ptt_held_on_entry_until_release_and_a_fresh_hold() {
        let mut policy = ChargingShutdown::new(true);
        for now in [0, 1_999, 2_000, 60_000] {
            assert_eq!(policy.update(now, Some(true), true), ChargingDecision::Stay);
        }
        assert_eq!(policy.update(60_001, Some(true), false), ChargingDecision::Stay);
        assert_eq!(policy.update(61_000, Some(true), true), ChargingDecision::Stay);
        assert_eq!(policy.update(62_999, Some(true), true), ChargingDecision::Stay);
        assert_eq!(policy.update(63_000, Some(true), true), ChargingDecision::RunNormally);
    }

    #[test]
    fn charging_shutdown_short_ptt_holds_do_not_accumulate() {
        let mut policy = ChargingShutdown::new(false);
        for start in [0, 2_000, 4_000] {
            assert_eq!(policy.update(start, Some(true), true), ChargingDecision::Stay);
            assert_eq!(policy.update(start + 1_999, Some(true), false), ChargingDecision::Stay);
        }
        assert_eq!(policy.update(6_000, None, true), ChargingDecision::Stay);
        assert_eq!(policy.update(7_999, None, true), ChargingDecision::Stay);
        assert_eq!(policy.update(8_000, None, true), ChargingDecision::RunNormally);
    }

    #[test]
    fn charging_shutdown_queued_short_release_cancels_before_a_late_poll() {
        let mut policy = ChargingShutdown::new(false);
        assert_eq!(policy.observe_ptt(0, true), ChargingDecision::Stay);
        assert_eq!(policy.update(1_900, Some(true), true), ChargingDecision::Stay);
        // Dequeued after 2 seconds, but the actual hold ended at 1,999 ms.
        assert_eq!(policy.observe_ptt(1_999, false), ChargingDecision::Stay);
        assert_eq!(policy.update(2_050, Some(true), false), ChargingDecision::Stay);
        assert_eq!(policy.update(60_000, Some(true), false), ChargingDecision::Stay);
    }

    #[test]
    fn charging_shutdown_release_and_repress_between_polls_start_a_new_hold() {
        let mut policy = ChargingShutdown::new(false);
        assert_eq!(policy.update(0, Some(true), true), ChargingDecision::Stay);
        assert_eq!(policy.update(1_900, Some(true), true), ChargingDecision::Stay);
        assert_eq!(policy.observe_ptt(1_999, false), ChargingDecision::Stay);
        assert_eq!(policy.observe_ptt(2_000, true), ChargingDecision::Stay);
        assert_eq!(policy.update(2_050, Some(true), true), ChargingDecision::Stay);
        assert_eq!(policy.update(3_999, Some(true), true), ChargingDecision::Stay);
        assert_eq!(policy.update(4_000, Some(true), true), ChargingDecision::RunNormally);
    }

    #[test]
    fn charging_shutdown_a_completed_hold_can_finish_on_its_release_stamp() {
        for start in [0u32, u32::MAX - 1_000] {
            for duration in [2_000, 2_500] {
                let mut policy = ChargingShutdown::new(false);
                assert_eq!(policy.update(start, Some(false), true), ChargingDecision::Stay);
                assert_eq!(
                    policy.observe_ptt(start.wrapping_add(duration), false),
                    ChargingDecision::RunNormally,
                );
                // PTT wins even if the charger deadline also elapsed while
                // these controls waited for the next app poll.
                assert_eq!(
                    policy.update(start.wrapping_add(3_000), Some(false), false),
                    ChargingDecision::RunNormally,
                );
            }
        }
    }

    #[test]
    fn charging_shutdown_queued_entry_held_release_only_arms_a_fresh_hold() {
        let mut policy = ChargingShutdown::new(true);
        assert_eq!(policy.observe_ptt(60_000, false), ChargingDecision::Stay);
        assert_eq!(policy.observe_ptt(60_001, true), ChargingDecision::Stay);
        assert_eq!(policy.observe_ptt(62_000, false), ChargingDecision::Stay);
        assert_eq!(policy.update(62_100, None, false), ChargingDecision::Stay);
        assert_eq!(policy.observe_ptt(63_000, true), ChargingDecision::Stay);
        assert_eq!(policy.observe_ptt(65_000, false), ChargingDecision::RunNormally);
    }

    #[test]
    fn charging_shutdown_ptt_edges_do_not_reset_the_charger_stop_timer() {
        let mut policy = ChargingShutdown::new(false);
        assert_eq!(policy.update(100, Some(false), false), ChargingDecision::Stay);
        assert_eq!(policy.observe_ptt(1_000, true), ChargingDecision::Stay);
        assert_eq!(policy.observe_ptt(1_500, false), ChargingDecision::Stay);
        assert_eq!(policy.update(2_100, Some(false), false), ChargingDecision::PowerOff);
        assert_eq!(policy.observe_ptt(3_000, true), ChargingDecision::PowerOff);
        assert_eq!(policy.observe_ptt(5_000, false), ChargingDecision::PowerOff);
    }

    #[test]
    fn charging_shutdown_stop_and_ptt_deadlines_survive_clock_wrap() {
        for start in [0u32, u32::MAX - 1_000] {
            for (charging, ptt, expected) in [
                (Some(false), false, ChargingDecision::PowerOff),
                (Some(true), true, ChargingDecision::RunNormally),
            ] {
                let mut policy = ChargingShutdown::new(false);
                assert_eq!(policy.update(start, charging, ptt), ChargingDecision::Stay);
                assert_eq!(policy.update(start.wrapping_add(1_999), charging, ptt), ChargingDecision::Stay);
                assert_eq!(policy.update(start.wrapping_add(2_000), charging, ptt), expected);
            }
        }
    }

    #[test]
    fn charging_shutdown_completed_ptt_hold_wins_a_simultaneous_stop_deadline() {
        let mut policy = ChargingShutdown::new(false);
        assert_eq!(policy.update(100, Some(false), true), ChargingDecision::Stay);
        assert_eq!(policy.update(2_099, Some(false), true), ChargingDecision::Stay);
        assert_eq!(policy.update(2_100, Some(false), true), ChargingDecision::RunNormally);
    }

    #[test]
    fn charging_shutdown_timers_are_independent_and_terminal_decisions_are_latched() {
        let mut resume = ChargingShutdown::new(false);
        assert_eq!(resume.update(0, Some(false), true), ChargingDecision::Stay);
        assert_eq!(resume.update(1_000, Some(true), true), ChargingDecision::Stay);
        assert_eq!(resume.update(2_000, Some(true), true), ChargingDecision::RunNormally);
        assert_eq!(resume.update(9_000, Some(false), false), ChargingDecision::RunNormally);
        assert_eq!(resume.update(11_000, Some(false), false), ChargingDecision::RunNormally);
        let mut stop = ChargingShutdown::new(false);
        assert_eq!(stop.update(0, Some(false), false), ChargingDecision::Stay);
        assert_eq!(stop.update(1_000, Some(false), true), ChargingDecision::Stay);
        assert_eq!(stop.update(1_999, Some(false), false), ChargingDecision::Stay);
        assert_eq!(stop.update(2_000, Some(false), false), ChargingDecision::PowerOff);
        assert_eq!(stop.update(9_000, Some(true), true), ChargingDecision::PowerOff);
        assert_eq!(stop.update(11_000, None, true), ChargingDecision::PowerOff);
    }

    #[test]
    fn power_on_is_unlocked() {
        let mut p = Policy::new(0);
        assert_eq!(p.lock_progress(), None);
        assert!(p.input(press(Button::PushToTalk), 1));
        assert!(p.input(release(Button::PushToTalk), 2));
    }
    #[test]
    fn idle_boundaries_and_clock_wrap() {
        for start in [0u32, u32::MAX - 1000] {
            let mut p = Policy::new(start);
            for (elapsed, expected) in [
                (DIM_AFTER_MS - 1, Screen::Active),
                (DIM_AFTER_MS, Screen::Dim),
                (OFF_AFTER_MS - 1, Screen::Dim),
                (OFF_AFTER_MS, Screen::Off),
            ] {
                assert_eq!(p.tick(start.wrapping_add(elapsed)), expected);
                assert_eq!(p.lock_progress().is_some(), expected == Screen::Off);
            }
        }
    }
    #[test]
    fn dim_controls_act_and_restore_brightness() {
        for input in [
            touch(TouchPhase::Down),
            press(Button::PushToTalk),
            Control::Wheel(Wheel::Ccw),
        ] {
            let mut p = Policy::new(0);
            p.tick(DIM_AFTER_MS);
            assert!(p.input(input, DIM_AFTER_MS + 1));
            assert_eq!(p.screen(), Screen::Active);
            assert_eq!(p.lock_progress(), None);
        }
    }
    #[test]
    fn all_six_buttons_wake_without_actions_or_releases() {
        for button in Button::ALL {
            let mut p = dark();
            assert!(!p.input(press(button), OFF_AFTER_MS + 1));
            assert!(p.is_down(button));
            assert_eq!(p.screen(), Screen::Active);
            p.frame_ready(OFF_AFTER_MS + 2);
            assert!(!p.input(release(button), OFF_AFTER_MS + 3));
            assert!(!p.held());
            assert_eq!(p.lock_progress(), Some(0));
        }
    }
    #[test]
    fn a_visible_frame_does_not_unlock_any_button() {
        let mut p = dark();
        p.wake(OFF_AFTER_MS + 1);
        p.frame_ready(OFF_AFTER_MS + 2);
        for button in Button::ALL {
            assert!(!p.input(press(button), OFF_AFTER_MS + 3));
            assert!(!p.input(release(button), OFF_AFTER_MS + 4));
        }
        assert_eq!(p.lock_progress(), Some(0));
    }
    #[test]
    fn fourth_counterclockwise_tick_unlocks_without_scrolling() {
        let mut p = dark();
        for tick in 1..=UNLOCK_TICKS {
            assert!(!p.input(Control::Wheel(Wheel::Ccw), OFF_AFTER_MS + tick as u32));
            assert_eq!(p.screen(), Screen::Active);
            assert_eq!(p.lock_progress(), (tick < UNLOCK_TICKS).then_some(tick));
        }
        p.frame_ready(OFF_AFTER_MS + 10);
        assert!(p.input(Control::Wheel(Wheel::Ccw), OFF_AFTER_MS + 11));
        assert!(p.input(press(Button::PushToTalk), OFF_AFTER_MS + 12));
    }
    #[test]
    fn clockwise_resets_unlock_progress() {
        let mut p = dark();
        for _ in 0..3 {
            assert!(!p.input(Control::Wheel(Wheel::Ccw), OFF_AFTER_MS + 1));
        }
        assert_eq!(p.lock_progress(), Some(3));
        assert!(!p.input(Control::Wheel(Wheel::Cw), OFF_AFTER_MS + 2));
        assert_eq!(p.lock_progress(), Some(0));
        unlock(&mut p, OFF_AFTER_MS + 3);
    }
    #[test]
    fn unrelated_buttons_cannot_complete_the_unlock_sequence() {
        let mut p = dark();
        p.input(Control::Wheel(Wheel::Ccw), OFF_AFTER_MS + 1);
        for button in Button::ALL {
            assert!(!p.input(press(button), OFF_AFTER_MS + 2));
            assert!(!p.input(release(button), OFF_AFTER_MS + 3));
        }
        assert_eq!(p.lock_progress(), Some(1));
    }
    #[test]
    fn held_locked_buttons_require_release_and_fresh_press_after_unlock() {
        for button in Button::ALL {
            let mut p = dark();
            assert!(!p.input(press(button), OFF_AFTER_MS + 1));
            unlock(&mut p, OFF_AFTER_MS + 2);
            p.frame_ready(OFF_AFTER_MS + 10);
            // A duplicate press / repeat while it is still down is not fresh.
            assert!(!p.input(press(button), OFF_AFTER_MS + 11));
            assert!(!p.input(release(button), OFF_AFTER_MS + 12));
            assert!(p.input(press(button), OFF_AFTER_MS + 13));
            assert!(p.input(release(button), OFF_AFTER_MS + 14));
        }
    }
    #[test]
    fn locked_touch_is_suppressed_through_unlock_and_release() {
        for end in [TouchPhase::Up, TouchPhase::Cancel] {
            let mut p = dark();
            assert!(!p.input(touch(TouchPhase::Down), OFF_AFTER_MS + 1));
            unlock(&mut p, OFF_AFTER_MS + 2);
            p.frame_ready(OFF_AFTER_MS + 10);
            assert!(!p.input(touch(TouchPhase::Move), OFF_AFTER_MS + 11));
            assert!(!p.input(touch(end), OFF_AFTER_MS + 12));
            assert!(p.input(touch(TouchPhase::Down), OFF_AFTER_MS + 13));
            assert!(p.input(touch(TouchPhase::Up), OFF_AFTER_MS + 14));
        }
    }
    #[test]
    fn repeated_locked_touch_contacts_never_activate() {
        let mut p = dark();
        for at in OFF_AFTER_MS..OFF_AFTER_MS + 5 {
            assert!(!p.input(touch(TouchPhase::Down), at));
            p.frame_ready(at);
            assert!(!p.input(touch(TouchPhase::Up), at));
        }
        assert_eq!(p.lock_progress(), Some(0));
    }
    #[test]
    fn trapped_ptt_cannot_keep_locked_screen_awake() {
        let mut p = dark();
        p.input(press(Button::PushToTalk), OFF_AFTER_MS + 1);
        assert_eq!(p.idle_ms(OFF_AFTER_MS + 101), 100);
        assert_eq!(p.tick(OFF_AFTER_MS + 1 + DIM_AFTER_MS), Screen::Dim);
        assert_eq!(p.tick(2 * OFF_AFTER_MS + 1), Screen::Off);
        assert_eq!(p.poll_ms(), DARK_POLL_MS);
        assert!(!p.input(release(Button::PushToTalk), 2 * OFF_AFTER_MS + 2));
        assert_eq!(p.screen(), Screen::Active);
        assert_eq!(p.lock_progress(), Some(0));
    }
    #[test]
    fn another_sleep_resets_partial_unlock() {
        let mut p = dark();
        p.input(Control::Wheel(Wheel::Ccw), OFF_AFTER_MS + 1);
        assert_eq!(p.lock_progress(), Some(1));
        p.tick(2 * OFF_AFTER_MS + 1);
        assert_eq!(p.lock_progress(), Some(0));
        unlock(&mut p, 2 * OFF_AFTER_MS + 2);
        p.frame_ready(2 * OFF_AFTER_MS + 10);
        p.tick(3 * OFF_AFTER_MS + 10);
        assert_eq!(p.lock_progress(), Some(0));
    }
    #[test]
    fn software_off_locks_even_if_panel_is_busy() {
        let mut p = Policy::new(0);
        p.tick(OFF_AFTER_MS);
        p.wake(OFF_AFTER_MS + 1);
        assert!(!p.input(press(Button::PushToTalk), OFF_AFTER_MS + 2));
        assert_eq!(p.lock_progress(), Some(0));
    }
    #[test]
    fn panel_sleep_alone_also_engages_lock() {
        let mut p = Policy::new(0);
        p.slept();
        assert_eq!(p.lock_progress(), Some(0));
    }
    #[test]
    fn raw_wake_never_unlocks() {
        let mut p = dark();
        p.wake(OFF_AFTER_MS + 1);
        p.frame_ready(OFF_AFTER_MS + 2);
        assert_eq!(p.screen(), Screen::Active);
        assert_eq!(p.lock_progress(), Some(0));
    }
    #[test]
    fn notifications_wake_without_unlocking_or_resetting_idle() {
        let mut p = dark();
        let at = OFF_AFTER_MS + 5000;
        p.notify(at);
        p.frame_ready(at);
        assert_eq!(p.tick(at + NOTIFY_MS - 1), Screen::Active);
        assert_eq!(p.idle_ms(at), at);
        assert!(!p.input(press(Button::PushToTalk), at + 1));
        assert!(!p.input(release(Button::PushToTalk), at + 2));
        assert_eq!(p.lock_progress(), Some(0));
        let mut p = dark();
        p.notify(at);
        assert_eq!(p.tick(at + NOTIFY_MS), Screen::Off);
    }
    #[test]
    fn notifications_from_dim_do_not_add_a_lock() {
        let mut p = Policy::new(0);
        p.tick(DIM_AFTER_MS);
        p.notify(DIM_AFTER_MS);
        assert_eq!(p.tick(DIM_AFTER_MS + NOTIFY_MS - 1), Screen::Active);
        assert_eq!(p.tick(DIM_AFTER_MS + NOTIFY_MS), Screen::Dim);
        assert_eq!(p.lock_progress(), None);
    }
    #[test]
    fn unlocking_before_wake_frame_does_not_exempt_ptt() {
        let mut p = dark();
        unlock(&mut p, OFF_AFTER_MS + 1);
        assert!(!p.input(press(Button::PushToTalk), OFF_AFTER_MS + 5));
        p.frame_ready(OFF_AFTER_MS + 10);
        assert!(!p.input(release(Button::PushToTalk), OFF_AFTER_MS + 11));
        assert!(p.input(press(Button::PushToTalk), OFF_AFTER_MS + 12));
    }
    #[test]
    fn queued_dark_presses_are_suppressed_after_unlock_including_clock_wrap() {
        for start in [0, u32::MAX - OFF_AFTER_MS - 100] {
            let mut p = Policy::new(start);
            let at = |ms| start.wrapping_add(OFF_AFTER_MS).wrapping_add(ms);
            p.tick(at(0));
            p.slept();
            unlock(&mut p, at(1));
            p.frame_ready(at(260));
            assert!(!p.input(press(Button::PushToTalk), at(180)));
            assert!(!p.input(release(Button::PushToTalk), at(200)));
            assert!(p.input(press(Button::PushToTalk), at(260)));
            assert!(p.input(release(Button::PushToTalk), at(300)));
            p.tick(at(260 + QUEUED_MS));
            assert!(p.input(press(Button::Enter), at(100)));
        }
    }
    #[test]
    fn controls_stamped_before_unlock_stay_suppressed_when_dequeued_late() {
        for start in [0, u32::MAX - OFF_AFTER_MS - 100] {
            let mut p = Policy::new(start);
            let at = |ms| start.wrapping_add(OFF_AFTER_MS).wrapping_add(ms);
            p.tick(at(0));
            p.slept();
            p.wake(at(1));
            p.frame_ready(at(5));
            unlock(&mut p, at(10));
            assert!(!p.input(press(Button::PushToTalk), at(12)));
            assert!(!p.input(release(Button::PushToTalk), at(20)));
            assert!(!p.input(Control::Wheel(Wheel::Ccw), at(12)));
            assert!(!p.input(touch(TouchPhase::Down), at(12)));
            assert!(!p.input(touch(TouchPhase::Up), at(20)));
            assert!(p.input(press(Button::PushToTalk), at(21)));
        }
    }

    #[test]
    fn synthetic_releases_do_not_keep_device_awake_or_unlock() {
        let mut p = Policy::new(0);
        for now in [1000, 10_000, 30_000, 100_000] {
            for button in Button::ALL {
                assert!(p.input(release(button), now));
            }
            p.input(touch(TouchPhase::Cancel), now);
        }
        assert_eq!(p.tick(OFF_AFTER_MS), Screen::Off);
        for button in Button::ALL {
            assert!(!p.input(release(button), OFF_AFTER_MS + 1));
        }
        assert_eq!(p.tick(OFF_AFTER_MS + 1), Screen::Off);
        assert_eq!(p.poll_ms(), DARK_POLL_MS);
    }
    #[test]
    fn unlocked_held_controls_keep_screen_on_and_track_idle() {
        for input in [press(Button::PushToTalk), touch(TouchPhase::Down)] {
            let mut p = Policy::new(100);
            assert!(p.input(input, 200));
            assert_eq!(p.idle_ms(OFF_AFTER_MS), 0);
            assert_eq!(p.tick(OFF_AFTER_MS), Screen::Active);
            assert_eq!(p.lock_progress(), None);
        }
        let mut p = Policy::new(0);
        p.input(press(Button::Enter), 1);
        p.input(release(Button::Enter), 2);
        assert_eq!(p.idle_ms(102), 100);
        let p = Policy::new(u32::MAX - 5);
        assert_eq!(p.idle_ms(4), 10);
    }
    #[test]
    fn held_buttons_are_independent() {
        let mut p = Policy::new(0);
        for button in Button::ALL {
            assert!(p.input(press(button), 1));
        }
        for button in Button::ALL {
            assert!(p.held());
            assert!(p.input(release(button), 2));
        }
        assert!(!p.held());
    }
    #[test]
    fn brightness_scales_with_user_level_and_dim_stays_below_it() {
        assert_eq!(brightness(Screen::Active, 5), FULL_BRIGHTNESS);
        assert_eq!(brightness(Screen::Active, 0), brightness(Screen::Active, 1));
        assert_eq!(brightness(Screen::Active, 9), FULL_BRIGHTNESS);
        for level in 1..5 {
            assert!(brightness(Screen::Active, level) < brightness(Screen::Active, level + 1));
            assert!(brightness(Screen::Dim, level) < brightness(Screen::Active, level));
            assert_eq!(brightness(Screen::Off, level), 0);
        }
        assert_eq!(brightness(Screen::Dim, 5), DIM_BRIGHTNESS);
    }
}
