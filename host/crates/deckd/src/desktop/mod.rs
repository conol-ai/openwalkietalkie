//! `deckd::desktop` (design §5.5, §2.3): what the daemon does on the Mac
//! itself, off the core thread — the hand-off and the Macropad keys — and
//! the seam the core reaches them through.
//!
//! - **The hand-off** ([`raise`]): a press on a session's `Open app` card
//!   brings forward the app the session runs in, if it is running, through
//!   LaunchServices. It replaced the Ghostty tab path, its AppleScript and
//!   its Automation grant; orders are posted by the session's inbox
//!   (`crate::inbox`) or Claude Channels (`crate::claude_channel`), which
//!   report through this seam too, and are never typed into a terminal.
//! - **Macropad keys** ([`keys`]): a key's keystroke, posted with the
//!   Accessibility grant.
//!
//! The seam is the one `deckd::hid` and `deckd::audio` already have, for
//! the same reason and with the same asymmetry: a **bounded** channel out
//! of the core that is only ever `try_send`, an **unbounded** one back, and
//! a handle the core owns. Every job here can block (`/usr/bin/open` is
//! given up to [`raise::RAISE_BUDGET`], a keystroke may put macOS's own
//! dialog up), and the core must never stop answering the deck for that.
//! So `Effect::Desktop` becomes a [`DesktopJob`] on a queue, a std thread
//! named `deckd-desktop` runs it, and the outcome comes back as
//! [`FromDesktop`] through the core's `select!`.
//!
//! **There may be no desktop at all.** `--no-device`, a non-macOS build and
//! every unit test that does not want one get [`DesktopHandle::none`]: a
//! hand-off or a keystroke is logged and forgotten. The outcome channel
//! exists on every handle, so the inbox and Claude Channels still report an
//! order's result in a `--no-device` daemon.
pub mod errors;
pub mod fakes;
pub mod keys;
pub mod raise;

pub use errors::DesktopError;
pub use raise::{Outcome, Raiser};

use crossbeam_channel::{Receiver, Sender, TrySendError};
use deck_model::AgentId;
use std::sync::Arc;

/// Capacity of the core → desktop channel.
///
/// Small, like the audio one and for the same reason: a job in the queue is
/// already late by however long the ones in front of it take, and a press
/// that waited behind thirty-two others is not one the operator still
/// means.
pub const CMDS_CAPACITY: usize = 32;

/// What the core asks the desktop side to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DesktopJob {
    /// Bring `app` (a bundle identifier) forward for `agent`'s hand-off.
    Raise { agent: AgentId, app: String },
    /// A Macropad key's keystroke, posted as it is (`keys::press`).
    Keystroke { keys: String },
    /// `perm.request{which:"accessibility"}`: read the grant, with macOS's
    /// prompt where prompts are allowed (once per run).
    Accessibility,
}

/// What the desktop side tells the core.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FromDesktop {
    /// An order's route finished: the session's inbox or Claude Channels
    /// (`crate::inbox::post_async`, `crate::claude_channel`). `Ok` means it
    /// was posted; `Err` is the `order.failed{reason}` the reducer
    /// publishes.
    Order {
        agent: AgentId,
        text: String,
        /// The order's id (`order.dispatched{order}`), which both routes
        /// carry back.
        order: Option<String>,
        result: Result<(), DesktopError>,
    },
    /// A hand-off finished: the app came forward, or why it did not. The
    /// deck hears of it only when it did not (`Core::on_from_desktop`).
    /// `app` is `None` for [`Outcome::NoApp`], which the core sends itself
    /// through [`DesktopHandle::outcomes`].
    Raised {
        agent: AgentId,
        app: Option<String>,
        outcome: Outcome,
    },
    /// The Accessibility grant, as the Macropad keys last learned it: a
    /// keystroke went out (`granted`) or macOS refused one (`denied`), or
    /// `perm.request` asked. `perm.status.accessibility` reads it.
    Accessibility { granted: bool },
}

/// What `deckd run` needs to start the desktop side.
#[derive(Clone, Debug, Default)]
pub struct DesktopConfig {
    /// May put macOS's own permission dialogs on the operator's screen (the
    /// Accessibility prompt, once per run, the first time a Macropad key
    /// is refused). `false` everywhere but the daemon.
    pub prompts: bool,
}

impl DesktopConfig {
    /// The daemon's own: the one place prompts are allowed.
    #[must_use]
    pub fn from_config(_cfg: &crate::config::Config) -> DesktopConfig {
        DesktopConfig { prompts: true }
    }
}

/// The desktop side itself: a raiser, the Accessibility grant as last
/// learned, and how keystrokes are posted.
///
/// A plain struct with no thread of its own, so every test drives it
/// directly — `on_job` in, `FromDesktop` out. [`run`] is the loop that
/// turns it into `deckd-desktop`.
pub struct Desktop {
    raiser: Arc<dyn Raiser>,
    cfg: DesktopConfig,
    /// The Accessibility grant as last learned (`None` until a key was
    /// pressed or `perm.request` asked); a change is reported to the core.
    accessibility: Option<bool>,
    /// The Accessibility prompt has been put up this run: once is a
    /// request, twice is nagging.
    prompted: bool,
    /// How a keystroke is posted: [`keys::press`], or a test's stand-in
    /// (a real press on the test machine would land in whatever has focus).
    press: fn(&str) -> Result<(), keys::KeyError>,
}

impl std::fmt::Debug for Desktop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Desktop")
            .field("raiser", &self.raiser.name())
            .field("accessibility", &self.accessibility)
            .finish()
    }
}

impl Desktop {
    /// The daemon's own: this platform's raiser.
    #[must_use]
    pub fn new(cfg: DesktopConfig) -> Desktop {
        Desktop::with(raise::default_raiser(), cfg)
    }

    /// A desktop over any raiser: what every test builds, over
    /// [`fakes::FakeRaiser`].
    #[must_use]
    pub fn with(raiser: Arc<dyn Raiser>, cfg: DesktopConfig) -> Desktop {
        Desktop {
            raiser,
            cfg,
            accessibility: None,
            prompted: false,
            press: keys::press,
        }
    }

    /// Post keystrokes through `press` instead of the desktop (tests).
    #[cfg(test)]
    fn set_presser(&mut self, press: fn(&str) -> Result<(), keys::KeyError>) {
        self.press = press;
    }

    /// Note what the Accessibility grant turned out to be; `true` when
    /// that is news for the core.
    fn learned_accessibility(&mut self, granted: bool) -> bool {
        let changed = self.accessibility != Some(granted);
        self.accessibility = Some(granted);
        changed
    }

    /// Ask macOS for the Accessibility grant: its own dialog, at most once
    /// per run and only where prompts are allowed; otherwise a plain read.
    fn ask_accessibility(&mut self) -> Option<bool> {
        let prompt = self.cfg.prompts && !self.prompted;
        if prompt {
            self.prompted = true;
            tracing::info!("asking macOS for the Accessibility grant (System Settings > Privacy & Security > Accessibility)");
        }
        keys::trusted(prompt)
    }

    /// Run one job.
    pub fn on_job(&mut self, job: DesktopJob) -> Vec<FromDesktop> {
        match job {
            DesktopJob::Raise { agent, app } => {
                let outcome = raise::hand_off(&*self.raiser, &app);
                vec![FromDesktop::Raised {
                    agent,
                    app: Some(app),
                    outcome,
                }]
            }
            DesktopJob::Keystroke { keys } => {
                let learned = match (self.press)(&keys) {
                    Ok(()) => {
                        tracing::debug!(%keys, "macro keystroke posted");
                        Some(true)
                    }
                    Err(keys::KeyError::NotTrusted) => {
                        // The refusal is the moment to ask: macOS lists the
                        // process under Accessibility and the operator
                        // switches it on; the next press goes through.
                        let _ = self.ask_accessibility();
                        tracing::warn!(%keys, error = %keys::KeyError::NotTrusted, "macro keystroke not posted");
                        Some(false)
                    }
                    Err(e) => {
                        tracing::warn!(%keys, error = %e, "macro keystroke not posted");
                        None
                    }
                };
                let mut out = Vec::new();
                if let Some(granted) = learned {
                    if self.learned_accessibility(granted) {
                        out.push(FromDesktop::Accessibility { granted });
                    }
                }
                out
            }
            DesktopJob::Accessibility => {
                let mut out = Vec::new();
                if let Some(granted) = self.ask_accessibility() {
                    if self.learned_accessibility(granted) {
                        out.push(FromDesktop::Accessibility { granted });
                    }
                }
                out
            }
        }
    }
}

/// The core's handle on the desktop side.
pub struct DesktopHandle {
    /// Bounded; `try_send` only. `None` when this build has no desktop.
    cmds: Option<Sender<DesktopJob>>,
    /// Unbounded: an outcome per job or posted order, and both are few.
    pub events: Receiver<FromDesktop>,
    /// A sender kept for the handle's whole life so `events` can never
    /// become `Disconnected` — a disconnected `select!` arm is always
    /// ready, and the core would spin at 100% CPU for the rest of the
    /// daemon's life (the same trap `AudioHandle` documents).
    _idle: Sender<FromDesktop>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for DesktopHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesktopHandle")
            .field("engine", &self.engine())
            .field("queued", &self.queued())
            .finish()
    }
}

impl DesktopHandle {
    /// The core's outcome channel, for a worker that is not the desktop
    /// thread: the inbox poster (`crate::inbox::post_async`) and Claude
    /// Channels report an order's result through the same
    /// [`FromDesktop::Order`], so `order.failed` has one path into the
    /// reducer. Exists on every handle, `none()` included — posting into a
    /// session's socket must work in a `--no-device` daemon.
    #[must_use]
    pub fn outcomes(&self) -> Sender<FromDesktop> {
        self._idle.clone()
    }

    /// No desktop: every job is logged and forgotten.
    #[must_use]
    pub fn none() -> DesktopHandle {
        let (idle, events) = crossbeam_channel::unbounded::<FromDesktop>();
        DesktopHandle {
            cmds: None,
            events,
            _idle: idle,
            join: None,
        }
    }

    /// A handle with no thread behind it: the caller holds the other ends,
    /// sees every job the core sends and can answer them. The bounded
    /// capacity is the real one, so a test can fill it.
    #[must_use]
    pub fn detached() -> (DesktopHandle, Receiver<DesktopJob>, Sender<FromDesktop>) {
        let (cmds_tx, cmds_rx) = crossbeam_channel::bounded::<DesktopJob>(CMDS_CAPACITY);
        let (events_tx, events_rx) = crossbeam_channel::unbounded::<FromDesktop>();
        (
            DesktopHandle {
                cmds: Some(cmds_tx),
                events: events_rx,
                _idle: events_tx.clone(),
                join: None,
            },
            cmds_rx,
            events_tx,
        )
    }

    #[must_use]
    pub fn engine(&self) -> bool {
        self.cmds.is_some()
    }

    #[must_use]
    pub fn queued(&self) -> usize {
        self.cmds.as_ref().map_or(0, crossbeam_channel::Sender::len)
    }

    /// `try_send`, never `send`: `None` when the desktop side took it, and
    /// **the job back** when it did not, so the caller can apply its own
    /// policy. A disconnected worker is a full one as far as that policy is
    /// concerned.
    #[must_use]
    pub fn send_or_return(&self, job: DesktopJob) -> Option<DesktopJob> {
        match self.cmds.as_ref() {
            None => Some(job),
            Some(tx) => match tx.try_send(job) {
                Ok(()) => None,
                Err(TrySendError::Full(job) | TrySendError::Disconnected(job)) => Some(job),
            },
        }
    }

    /// Dropping the job sender ends the thread; join it.
    pub fn join(mut self) {
        drop(self.cmds.take());
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Default for DesktopHandle {
    fn default() -> Self {
        DesktopHandle::none()
    }
}

/// Start `deckd-desktop`.
///
/// A std thread and not a tokio task, for the reason §2.3 gives the audio
/// control loop: everything it does can block (a child `/usr/bin/open`, a
/// keystroke that puts macOS's prompt up), and a blocking job on a tokio
/// worker stalls every door behind it.
#[must_use]
pub fn spawn(cfg: DesktopConfig) -> DesktopHandle {
    let (cmds_tx, cmds_rx) = crossbeam_channel::bounded::<DesktopJob>(CMDS_CAPACITY);
    let (events_tx, events_rx) = crossbeam_channel::unbounded::<FromDesktop>();
    let desktop = Desktop::new(cfg);
    let idle = events_tx.clone();
    let join = std::thread::Builder::new()
        .name("deckd-desktop".into())
        .spawn(move || run(desktop, &cmds_rx, &events_tx))
        .ok();
    DesktopHandle {
        cmds: Some(cmds_tx),
        events: events_rx,
        _idle: idle,
        join,
    }
}

/// The `deckd-desktop` loop: one job at a time, in arrival order, until the
/// core drops its sender.
pub fn run(mut desktop: Desktop, jobs: &Receiver<DesktopJob>, out: &Sender<FromDesktop>) {
    while let Ok(job) = jobs.recv() {
        for event in desktop.on_job(job) {
            if out.send(event).is_err() {
                return; // the core is gone
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::fakes::FakeRaiser;

    fn agent() -> AgentId {
        AgentId("ag_cd34".into())
    }

    fn raise(app: &str) -> DesktopJob {
        DesktopJob::Raise {
            agent: agent(),
            app: app.into(),
        }
    }

    /// A raise job reports what it came to, for the agent and app it named.
    #[test]
    fn a_raise_job_reports_its_outcome() {
        let fake = Arc::new(FakeRaiser::new().with_running("dev.zed.Zed"));
        let mut desktop = Desktop::with(fake.clone(), DesktopConfig::default());
        assert_eq!(
            desktop.on_job(raise("dev.zed.Zed")),
            [FromDesktop::Raised {
                agent: agent(),
                app: Some("dev.zed.Zed".into()),
                outcome: Outcome::Raised,
            }]
        );
        assert_eq!(
            desktop.on_job(raise("com.mitchellh.ghostty")),
            [FromDesktop::Raised {
                agent: agent(),
                app: Some("com.mitchellh.ghostty".into()),
                outcome: Outcome::NotRunning,
            }]
        );
        assert_eq!(fake.raised(), vec!["dev.zed.Zed".to_string()]);
    }

    /// A Macropad key's keystroke reports the Accessibility grant as it
    /// learns it: a refusal is `denied` (and, where prompts are allowed,
    /// the moment macOS is asked), a press that went out is `granted`, an
    /// unknown combo says nothing, and the same answer twice is reported
    /// once.
    #[test]
    fn a_keystroke_reports_the_accessibility_grant_once_per_change() {
        let mut desktop = Desktop::with(Arc::new(FakeRaiser::new()), DesktopConfig::default());
        fn refused(_: &str) -> Result<(), keys::KeyError> {
            Err(keys::KeyError::NotTrusted)
        }
        fn posted(_: &str) -> Result<(), keys::KeyError> {
            Ok(())
        }
        fn unknown(_: &str) -> Result<(), keys::KeyError> {
            Err(keys::KeyError::Unknown("cmd+ptt".into()))
        }
        let press = || DesktopJob::Keystroke {
            keys: "cmd+c".into(),
        };
        desktop.set_presser(refused);
        assert_eq!(
            desktop.on_job(press()),
            [FromDesktop::Accessibility { granted: false }]
        );
        assert!(
            desktop.on_job(press()).is_empty(),
            "the same news is not repeated"
        );
        desktop.set_presser(unknown);
        assert!(
            desktop.on_job(press()).is_empty(),
            "an unknown combo says nothing about the grant"
        );
        desktop.set_presser(posted);
        assert_eq!(
            desktop.on_job(press()),
            [FromDesktop::Accessibility { granted: true }]
        );
        assert!(!desktop.prompted, "no prompt in a test binary");
    }

    /// The two states a handle can be in are genuinely different: "there
    /// is nothing to send it to" hands the job back like a full queue does,
    /// and only a real queue can fill.
    #[test]
    fn a_handle_with_no_desktop_refuses_without_pretending_to_be_full() {
        let idle = DesktopHandle::none();
        assert!(!idle.engine());
        assert_eq!(idle.queued(), 0);
        let job = DesktopJob::Keystroke {
            keys: "cmd+c".into(),
        };
        assert_eq!(idle.send_or_return(job.clone()), Some(job.clone()));

        let (desktop, jobs, events) = DesktopHandle::detached();
        assert!(desktop.engine());
        for _ in 0..CMDS_CAPACITY {
            assert!(desktop.send_or_return(job.clone()).is_none());
        }
        assert_eq!(desktop.queued(), CMDS_CAPACITY);
        assert_eq!(
            desktop.send_or_return(DesktopJob::Accessibility),
            Some(DesktopJob::Accessibility),
            "bounded, and the refused job comes back"
        );
        assert_eq!(jobs.recv(), Ok(job.clone()));
        assert!(desktop.send_or_return(job).is_none());
        // Dropping every outside sender must leave the receiver **empty**
        // and not **disconnected**: a disconnected `select!` arm is always
        // ready and the core loop would spin for the daemon's whole life.
        events
            .send(FromDesktop::Accessibility { granted: true })
            .unwrap();
        assert_eq!(
            desktop.events.recv(),
            Ok(FromDesktop::Accessibility { granted: true })
        );
        drop(events);
        assert_eq!(
            desktop.events.try_recv(),
            Err(crossbeam_channel::TryRecvError::Empty)
        );
        assert_eq!(
            DesktopHandle::none().events.try_recv(),
            Err(crossbeam_channel::TryRecvError::Empty)
        );
    }

    /// The worker loop runs raise jobs in order and ends when the core
    /// drops its sender.
    #[test]
    fn the_worker_thread_runs_raise_jobs_in_order() {
        let fake = Arc::new(
            FakeRaiser::new()
                .with_running("dev.zed.Zed")
                .with_running("com.mitchellh.ghostty"),
        );
        let desktop = Desktop::with(fake.clone(), DesktopConfig::default());
        let (jobs_tx, jobs_rx) = crossbeam_channel::bounded::<DesktopJob>(CMDS_CAPACITY);
        let (out_tx, out_rx) = crossbeam_channel::unbounded::<FromDesktop>();
        let worker = std::thread::spawn(move || run(desktop, &jobs_rx, &out_tx));
        jobs_tx.send(raise("dev.zed.Zed")).unwrap();
        jobs_tx.send(raise("com.mitchellh.ghostty")).unwrap();
        drop(jobs_tx);
        worker.join().unwrap();
        let apps: Vec<String> = out_rx
            .try_iter()
            .filter_map(|e| match e {
                FromDesktop::Raised {
                    app,
                    outcome: Outcome::Raised,
                    ..
                } => app,
                _ => None,
            })
            .collect();
        assert_eq!(apps, ["dev.zed.Zed", "com.mitchellh.ghostty"]);
        assert_eq!(fake.raised(), apps);
    }

    /// Only the daemon may put macOS's own dialogs up; a test's desktop
    /// never does.
    #[test]
    fn the_desktop_config_prompts_only_in_the_daemon() {
        assert!(DesktopConfig::from_config(&crate::config::Config::default()).prompts);
        assert!(!DesktopConfig::default().prompts);
    }
}
