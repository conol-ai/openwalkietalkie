//! When the service looks for a release by itself (R8): the firmware and
//! app update checks run in the daemon, so they go on with the window
//! closed and with the app not running at all.
//!
//! The first look comes five minutes ([`GRACE`]) after the service starts,
//! so a login or a restart does not reach for the network at once. After
//! that the service looks again six hours ([`EVERY`]) after the release feed
//! last answered, and an hour ([`RETRY`]) after a look that could not reach
//! it. It looks only while a deck was linked in the last ten minutes
//! (`crate::dormancy`): a Mac with no deck around sends nothing for it, so
//! the quiet's "no child processes" holds for the look's `curl` too.
//! `[updates] check = false` stops it; the owner's **Check** still looks.
//!
//! A pure clock: the core tells it the time and what it knows
//! (`Core::check_for_updates`), so a test walks six hours in no time at all.
//! The intervals count `Instant`s, which stop while the Mac sleeps: six
//! hours means six hours awake.
use std::time::{Duration, Instant};

/// How long after the service starts the first look waits.
pub const GRACE: Duration = Duration::from_secs(5 * 60);

/// How long a look that reached the release feed stands.
pub const EVERY: Duration = super::CHECK_INTERVAL;

/// How long after a look that could not reach the feed the next one waits,
/// and the least time between two looks whatever came of the first.
pub const RETRY: Duration = Duration::from_secs(60 * 60);

/// The looks the service started by itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Schedule {
    /// When the last one started, whatever came of it.
    last: Option<Instant>,
    /// How many there were.
    looks: u32,
}

impl Schedule {
    /// Whether to look now, for a service started at `started`: a deck was
    /// linked in the last ten minutes (`deck_recent`), the grace after the
    /// start is over, the feed's last answer (`answered`) is six hours old
    /// and the last look that could not reach it (`failed`) an hour old.
    /// Never sooner than an hour after the schedule's own last look, so a
    /// look that started nothing (an updater that never reaches out) is not
    /// asked again on every tick.
    #[must_use]
    pub fn due(
        &self,
        now: Instant,
        started: Instant,
        deck_recent: bool,
        answered: Option<Instant>,
        failed: Option<Instant>,
    ) -> bool {
        let over = |at: Option<Instant>, wait: Duration| {
            at.is_none_or(|at| now.saturating_duration_since(at) >= wait)
        };
        deck_recent
            && over(Some(started), GRACE)
            && over(answered, EVERY)
            && over(failed, RETRY)
            && over(self.last, RETRY)
    }

    /// A look started at `now`.
    pub fn ran(&mut self, now: Instant) {
        self.last = Some(now);
        self.looks = self.looks.saturating_add(1);
    }

    /// How many looks the service started by itself.
    #[must_use]
    pub fn looks(&self) -> u32 {
        self.looks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minutes(m: u64) -> Duration {
        Duration::from_secs(m * 60)
    }

    #[test]
    fn nothing_before_the_grace() {
        let t0 = Instant::now();
        let schedule = Schedule::default();
        assert!(!schedule.due(t0, t0, true, None, None), "not at the start");
        let almost = t0 + GRACE - Duration::from_secs(1);
        assert!(!schedule.due(almost, t0, true, None, None));
        assert!(schedule.due(t0 + GRACE, t0, true, None, None));
        assert_eq!(GRACE, minutes(5));
    }

    /// The deck predicate is R5's (`Dormancy::deck_recent`), passed in.
    #[test]
    fn only_with_a_deck_linked_in_the_last_ten_minutes() {
        let t0 = Instant::now();
        let schedule = Schedule::default();
        let later = t0 + minutes(30);
        assert!(!schedule.due(later, t0, false, None, None));
        assert!(schedule.due(later, t0, true, None, None));
    }

    #[test]
    fn every_six_hours_after_a_success_and_an_hour_after_a_failure() {
        let t0 = Instant::now();
        let mut schedule = Schedule::default();
        let first = t0 + GRACE;
        schedule.ran(first);
        // The feed answered: six hours from that answer.
        let answered = first + Duration::from_secs(2);
        let six = answered + EVERY;
        let before = six - Duration::from_secs(1);
        assert!(!schedule.due(before, t0, true, Some(answered), None));
        assert!(schedule.due(six, t0, true, Some(answered), None));
        // It could not reach the feed: an hour from that, though the last
        // answer is older than six hours.
        schedule.ran(six);
        let failed = six + Duration::from_secs(15);
        let hour = failed + RETRY;
        assert!(!schedule.due(
            hour - Duration::from_secs(1),
            t0,
            true,
            Some(answered),
            Some(failed)
        ));
        assert!(schedule.due(hour, t0, true, Some(answered), Some(failed)));
        // An answer after the failure: six hours from the answer.
        schedule.ran(hour);
        let fed = hour + Duration::from_secs(3);
        assert!(!schedule.due(fed + RETRY, t0, true, Some(fed), Some(failed)));
        assert!(schedule.due(fed + EVERY, t0, true, Some(fed), Some(failed)));
        assert_eq!(schedule.looks(), 3);
        assert_eq!((EVERY, RETRY), (minutes(360), minutes(60)));
    }

    /// An updater that never reaches out (`Updater::offline`) leaves no
    /// answer and no failure: the schedule's own last look still spaces the
    /// next one an hour out.
    #[test]
    fn an_offline_updater_is_not_asked_every_tick() {
        let t0 = Instant::now();
        let mut schedule = Schedule::default();
        let first = t0 + GRACE;
        assert!(schedule.due(first, t0, true, None, None));
        schedule.ran(first);
        for later in [Duration::from_millis(250), minutes(1), minutes(59)] {
            assert!(
                !schedule.due(first + later, t0, true, None, None),
                "{later:?}"
            );
        }
        assert!(schedule.due(first + RETRY, t0, true, None, None));
        assert_eq!(schedule.looks(), 1);
    }

    /// A clock that reads earlier than a stamp (an `Instant` from another
    /// thread, read a moment later) is no time at all, never a panic.
    #[test]
    fn a_stamp_after_now_counts_as_just_now() {
        let t0 = Instant::now();
        let schedule = Schedule::default();
        let now = t0 + minutes(10);
        let ahead = Some(now + Duration::from_millis(5));
        assert!(!schedule.due(now, t0, true, ahead, None));
        assert!(!schedule.due(now, t0, true, None, ahead));
    }
}
