//! Dormancy: it goes quiet without a deck.
//!
//! With no deck linked for ten minutes the service stops collecting from
//! agent sessions: no session polling, no transcript watchers, no usage
//! reads (so no Keychain read and no `curl`), no child processes. It starts
//! again the moment a deck links. The window is ten minutes because the
//! link can drop for seconds at a time, and a deck that comes back from a
//! walk to the kitchen should find the roster as it left it.
//!
//! The core decides ([`Dormancy`], told on every check whether the deck is
//! linked and what time it is) and publishes a [`Gate`] through
//! `adapters::rt::Adapters`, which every collector and both hook doors
//! already hold. Each reads the gate before it acts: the liveness pollers
//! (`crate::liveness`), the Codex app-server client (`crate::codexd`), the
//! Claude usage probe (`crate::usage::claude`), the System sampler
//! (`crate::system_status`), the transcript watchers (`crate::chat`) and the
//! hook doors (`api::hooks`). Two of the gate's switches follow the deck's
//! strip as well: the account's usage is asked for only while the Usage face
//! is on it, and the Mac's load sampled only while the System face is.
//!
//! When the collectors stop, the core releases the agents the Claude and
//! Codex adapters own (`agent.removed{reason:"dormant"}`), and [`follow`]
//! drops the transcript watchers and the Claude adapter's memo of their
//! cards. When a deck links they come back the way they come at start-up:
//! the roster seed, Codex's loaded threads, the next hook.
//!
//! What keeps running: deck discovery over HID and Bluetooth (it is how the
//! deck comes back), the Claude Channels door's look at its own switch file,
//! the config watcher and the core's ticker. Connected apps are served as
//! before; one that collects too (the Omarchy bridge) follows `device.link`
//! or `service.dormancy` itself.
//!
//! A daemon with no deck to wait for (`--no-device`) has no dormancy: its
//! gate is always open, so every door test and the gate's `api` step run as
//! they always did.
//!
//! **The clock.** The window counts the core's `Instant`s, which on macOS
//! do not advance while the Mac sleeps: a Mac put to sleep with the deck away
//! finishes the window after it wakes. Only start-up compares wall clocks,
//! the deck's last link as `state.json` kept it against the time now.
use crate::api::Api;
use deck_model::{DormancyState, Snapshot, Template, Ts};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// How long the collectors keep running after the deck was last linked.
pub const WINDOW: Duration = Duration::from_secs(600);

/// The `agent.removed` reason of an agent released because the service went
/// quiet.
pub const DORMANT: &str = "dormant";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// No dormancy: the collectors always run.
    Off,
    /// A deck is linked.
    Linked,
    /// No deck; the collectors run until `until`.
    Lingering { until: Instant },
    /// No deck for the whole window. `since` is when the collectors
    /// stopped, `None` when no deck was ever linked to this service.
    Dormant { since: Option<Ts> },
}

/// Whether the collectors run: a pure function of the link and the clock
/// it is told, so a test walks ten minutes in no time at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dormancy {
    window: Duration,
    phase: Phase,
}

impl Default for Dormancy {
    fn default() -> Self {
        Dormancy::off()
    }
}

impl Dormancy {
    /// No dormancy: `--no-device`, and every core nothing has started one
    /// for.
    #[must_use]
    pub fn off() -> Self {
        Dormancy {
            window: WINDOW,
            phase: Phase::Off,
        }
    }

    /// The dormancy of a service starting at `wall` (`now` on the core's
    /// clock), whose deck was last linked at `last`, as `state.json` kept
    /// it. No deck ever linked: quiet from the start. One linked inside the
    /// window: the collectors run for the rest of it. One linked longer ago:
    /// quiet since the window ran out. A wall clock that is behind `last`
    /// counts as no time at all.
    #[must_use]
    pub fn start(window: Duration, last: Option<Ts>, wall: Ts, now: Instant) -> Self {
        let phase = match last {
            None => Phase::Dormant { since: None },
            Some(last) => {
                let elapsed = Duration::from_millis(wall.millis().saturating_sub(last.millis()));
                match window.checked_sub(elapsed).filter(|left| !left.is_zero()) {
                    Some(left) => Phase::Lingering { until: now + left },
                    None => Phase::Dormant {
                        since: Some(last.plus(window)),
                    },
                }
            }
        };
        Dormancy { window, phase }
    }

    /// One look at the link: `linked` is whether the deck counts this
    /// computer as present (`device.host_online`), `wall` the service's
    /// clock at `now`. A link ends the quiet at once; one that drops starts
    /// the window; the window running out stops the collectors, as of the
    /// moment it ran out. Whether that changed [`Dormancy::collecting`].
    pub fn observe(&mut self, linked: bool, wall: Ts, now: Instant) -> bool {
        let was = self.collecting();
        self.phase = match self.phase {
            Phase::Off => Phase::Off,
            _ if linked => Phase::Linked,
            Phase::Linked => Phase::Lingering {
                until: now + self.window,
            },
            Phase::Lingering { until } if now >= until => {
                // A check that came late still says when the window ran out.
                let late = now.saturating_duration_since(until).as_millis() as u64;
                Phase::Dormant {
                    since: Some(Ts::from_millis(wall.millis().saturating_sub(late))),
                }
            }
            phase => phase,
        };
        was != self.collecting()
    }

    /// Whether this service has a dormancy at all.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.phase != Phase::Off
    }

    /// Whether the collectors run.
    #[must_use]
    pub fn collecting(&self) -> bool {
        !matches!(self.phase, Phase::Dormant { .. })
    }

    /// A deck was linked in the last ten minutes: the collectors' rule,
    /// without the service that has no dormancy, which has no deck to wait
    /// for (`--no-device`). What the service's own looks for a release wait
    /// on (`firmware::Schedule`, R8).
    #[must_use]
    pub fn deck_recent(&self) -> bool {
        self.enabled() && self.collecting()
    }

    /// The snapshot's `service.dormancy`: `None` for no dormancy.
    #[must_use]
    pub fn state(&self) -> Option<DormancyState> {
        let window_s = u32::try_from(self.window.as_secs()).unwrap_or(u32::MAX);
        match self.phase {
            Phase::Off => None,
            Phase::Dormant { since } => Some(DormancyState {
                window_s,
                collecting: false,
                dormant_since: since,
            }),
            Phase::Linked | Phase::Lingering { .. } => Some(DormancyState {
                window_s,
                collecting: true,
                dormant_since: None,
            }),
        }
    }
}

/// What the collectors may do now: the one value every one of them reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gate {
    /// The collectors run: a deck was linked in the last ten minutes, or
    /// the service has no dormancy.
    pub collect: bool,
    /// The account's usage is asked for: collecting, and the Usage face is
    /// on the deck's strip.
    pub usage: bool,
    /// The Mac's load is sampled: collecting, and the System face is on the
    /// deck's strip.
    pub system: bool,
}

impl Gate {
    /// Everything runs: a service with no dormancy.
    #[must_use]
    pub const fn always() -> Self {
        Gate {
            collect: true,
            usage: true,
            system: true,
        }
    }

    /// Nothing runs.
    #[must_use]
    pub const fn shut() -> Self {
        Gate {
            collect: false,
            usage: false,
            system: false,
        }
    }

    /// The gate `dormancy` and the deck's strip in `snapshot` make.
    #[must_use]
    pub fn of(dormancy: &Dormancy, snapshot: &Snapshot) -> Self {
        if !dormancy.enabled() {
            return Gate::always();
        }
        let collect = dormancy.collecting();
        Gate {
            collect,
            usage: collect && on_strip(snapshot, Template::Usage),
            system: collect && on_strip(snapshot, Template::System),
        }
    }
}

/// Whether a face of `template` is on the deck's strip: the strip the deck
/// last reported (its `hello` and every edit), else the one the service
/// keeps for it. A deck that booted fresh shows its own strip, which the
/// service only learns from the deck.
#[must_use]
pub fn on_strip(snapshot: &Snapshot, template: Template) -> bool {
    let strip = if snapshot.device.faces.is_empty() {
        &snapshot.faces
    } else {
        &snapshot.device.faces
    };
    strip.iter().any(|face| face.t == template)
}

/// The gate's next change. A gate whose sender is gone never changes again:
/// the sender lives in `rt::Adapters`, which outlives every collector, so
/// that is only ever the daemon on its way out.
pub async fn changed(gate: &mut watch::Receiver<Gate>) {
    if gate.changed().await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Until the collectors run (`want`) or stop (`!want`): at once when they
/// already do.
pub async fn until_collect(gate: &mut watch::Receiver<Gate>, want: bool) {
    if gate.wait_for(|gate| gate.collect == want).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// The doors' half of the quiet, until `stop`: every time the collectors
/// stop, [`quiet`] the adapters.
pub async fn follow(api: Arc<Api>, mut stop: watch::Receiver<bool>) {
    let mut gate = api.core.adapters().watch_gate();
    loop {
        tokio::select! {
            _ = stop.changed() => return,
            () = until_collect(&mut gate, false) => {}
        }
        tracing::debug!("dormancy: the transcript watchers and the claude memo go");
        quiet(&api.adapters);
        tokio::select! {
            _ = stop.changed() => return,
            () = until_collect(&mut gate, true) => {}
        }
    }
}

/// Drop what the doors keep about the sessions the core has just released:
/// the transcript watchers (dropping one aborts its task), the transcripts
/// their hooks named (`crate::reader`) and the Claude adapter's memo of
/// their agents and cards, which would otherwise read a released agent's
/// stale card as still on the deck and keep a later one off it. The
/// adapter's own session stays.
pub fn quiet(adapters: &crate::adapters::Adapters) {
    if let Ok(mut watches) = adapters.transcripts.lock() {
        watches.clear();
    }
    adapters.forget_transcripts(|_| false);
    adapters.claude.reset();
}

#[cfg(test)]
mod tests {
    use super::*;
    use deck_model::FaceEntry;

    const WALL: Ts = Ts(1_789_760_523_000);

    fn minutes(m: u64) -> Duration {
        Duration::from_secs(m * 60)
    }

    /// The wall clock `elapsed` after `WALL`.
    fn wall(elapsed: Duration) -> Ts {
        WALL.plus(elapsed)
    }

    /// A service started with a deck linked a minute ago keeps collecting
    /// for the nine minutes left, and stops as of the moment they ran out.
    #[test]
    fn a_start_inside_the_window_collects_until_the_window_runs_out() {
        let t0 = Instant::now();
        let last = WALL;
        let mut d = Dormancy::start(WINDOW, Some(last), wall(minutes(1)), t0);
        assert!(d.enabled() && d.collecting());
        assert_eq!(
            d.state(),
            Some(DormancyState {
                window_s: 600,
                collecting: true,
                dormant_since: None
            })
        );
        let left = minutes(9);
        assert!(!d.observe(
            false,
            wall(minutes(1) + left),
            t0 + left - Duration::from_millis(1)
        ));
        assert!(d.collecting());
        assert!(d.observe(false, wall(minutes(1) + left), t0 + left));
        assert!(!d.collecting());
        assert_eq!(d.state().unwrap().dormant_since, Some(last.plus(WINDOW)));
    }

    #[test]
    fn a_service_whose_deck_was_last_linked_long_ago_or_never_starts_quiet() {
        let t0 = Instant::now();
        let d = Dormancy::start(WINDOW, Some(WALL), wall(minutes(60)), t0);
        assert!(!d.collecting());
        assert_eq!(d.state().unwrap().dormant_since, Some(WALL.plus(WINDOW)));
        // Exactly the window ago is out of it.
        let d = Dormancy::start(WINDOW, Some(WALL), wall(WINDOW), t0);
        assert!(!d.collecting());
        // Never linked: quiet, and no time to say.
        let d = Dormancy::start(WINDOW, None, WALL, t0);
        assert!(!d.collecting());
        assert_eq!(
            d.state(),
            Some(DormancyState {
                window_s: 600,
                collecting: false,
                dormant_since: None
            })
        );
    }

    /// A wall clock set back behind the last link is no time at all: the
    /// whole window, never a stop at once.
    #[test]
    fn a_clock_behind_the_last_link_counts_as_no_time() {
        let t0 = Instant::now();
        let mut d = Dormancy::start(WINDOW, Some(wall(minutes(30))), WALL, t0);
        assert!(d.collecting());
        assert!(!d.observe(false, WALL, t0 + WINDOW - Duration::from_secs(1)));
        assert!(d.observe(false, WALL, t0 + WINDOW));
    }

    /// The link drops: nine minutes and fifty-nine seconds still collect,
    /// ten minutes do not, and a check that comes late still dates the stop
    /// to when the window ran out.
    #[test]
    fn a_drop_lingers_for_the_window_then_goes_quiet() {
        let t0 = Instant::now();
        let mut d = Dormancy::start(WINDOW, None, WALL, t0);
        assert!(
            d.observe(true, WALL, t0),
            "a deck links: collecting at once"
        );
        assert!(d.collecting());
        let gone = t0 + minutes(3);
        assert!(!d.observe(false, wall(minutes(3)), gone));
        let almost = WINDOW - Duration::from_secs(1);
        assert!(!d.observe(false, wall(minutes(3) + almost), gone + almost));
        assert!(d.collecting());
        let late = Duration::from_millis(250);
        assert!(d.observe(false, wall(minutes(13)).plus(late), gone + WINDOW + late));
        assert!(!d.collecting());
        assert_eq!(d.state().unwrap().dormant_since, Some(wall(minutes(13))));
        // Quiet stays quiet, however long.
        assert!(!d.observe(false, wall(minutes(90)), gone + minutes(87)));
    }

    #[test]
    fn a_link_inside_the_window_never_goes_quiet_and_one_after_it_wakes_at_once() {
        let t0 = Instant::now();
        let mut d = Dormancy::start(WINDOW, None, WALL, t0);
        d.observe(true, WALL, t0);
        d.observe(false, WALL, t0 + minutes(1));
        // Back after nine minutes: the window starts over at the next drop.
        assert!(!d.observe(true, wall(minutes(10)), t0 + minutes(10)));
        assert!(!d.observe(false, wall(minutes(11)), t0 + minutes(11)));
        assert!(!d.observe(false, wall(minutes(20)), t0 + minutes(20)));
        assert!(
            d.collecting(),
            "ten minutes since the second drop have not passed"
        );
        assert!(d.observe(false, wall(minutes(21)), t0 + minutes(21)));
        // A deck linking ends the quiet on the spot.
        assert!(d.observe(true, wall(minutes(40)), t0 + minutes(40)));
        assert_eq!(
            d.state().unwrap(),
            DormancyState {
                window_s: 600,
                collecting: true,
                dormant_since: None
            }
        );
    }

    /// R8's look for a release waits on a deck in the last ten minutes:
    /// the collectors' window, and never for a service with no deck to
    /// wait for.
    #[test]
    fn a_recent_deck_is_one_linked_inside_the_window() {
        let t0 = Instant::now();
        let mut d = Dormancy::start(WINDOW, None, WALL, t0);
        assert!(!d.deck_recent(), "never linked");
        d.observe(true, WALL, t0);
        assert!(d.deck_recent());
        d.observe(false, WALL, t0 + minutes(1));
        d.observe(false, wall(minutes(10)), t0 + minutes(10));
        assert!(d.deck_recent(), "nine minutes after the drop");
        d.observe(false, wall(minutes(11)), t0 + minutes(11));
        assert!(!d.deck_recent());
        let mut off = Dormancy::off();
        off.observe(true, WALL, t0);
        assert!(off.collecting() && !off.deck_recent());
    }

    #[test]
    fn no_dormancy_always_collects_whatever_the_link() {
        let t0 = Instant::now();
        let mut d = Dormancy::off();
        assert!(!d.enabled());
        assert!(!d.observe(false, WALL, t0));
        assert!(!d.observe(false, WALL, t0 + minutes(600)));
        assert!(d.collecting());
        assert_eq!(d.state(), None);
        assert_eq!(Gate::of(&d, &Snapshot::default()), Gate::always());
    }

    /// Usage and the System sampler follow the strip: the deck's own when it
    /// has reported one, else the service's; and neither runs while the
    /// collectors do not.
    #[test]
    fn usage_and_system_follow_the_faces_on_the_strip() {
        let face = |f, t| FaceEntry {
            f,
            t,
            n: String::new(),
        };
        let t0 = Instant::now();
        let mut linked = Dormancy::start(WINDOW, None, WALL, t0);
        linked.observe(true, WALL, t0);
        let mut snapshot = Snapshot::default();
        assert_eq!(
            Gate::of(&linked, &snapshot),
            Gate {
                collect: true,
                usage: false,
                system: false
            }
        );
        snapshot.faces = vec![face(1, Template::Agents), face(4, Template::Usage)];
        assert_eq!(
            Gate::of(&linked, &snapshot),
            Gate {
                collect: true,
                usage: true,
                system: false
            }
        );
        // The deck's own report wins over the service's copy.
        snapshot.device.faces = vec![face(1, Template::Agents), face(7, Template::System)];
        assert_eq!(
            Gate::of(&linked, &snapshot),
            Gate {
                collect: true,
                usage: false,
                system: true
            }
        );
        let quiet = Dormancy::start(WINDOW, None, WALL, t0);
        assert_eq!(Gate::of(&quiet, &snapshot), Gate::shut());
    }

    #[tokio::test]
    async fn waiting_on_the_gate_returns_at_once_when_it_already_says_so() {
        let (tx, mut rx) = watch::channel(Gate::shut());
        until_collect(&mut rx, false).await;
        let waiter = tokio::spawn(async move {
            until_collect(&mut rx, true).await;
            rx
        });
        tx.send_replace(Gate::always());
        let mut rx = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("woken by the change")
            .unwrap();
        // A gate whose sender is gone never changes again.
        drop(tx);
        let parked = tokio::time::timeout(Duration::from_millis(50), changed(&mut rx)).await;
        assert!(parked.is_err(), "it waits for ever rather than spinning");
    }
}
