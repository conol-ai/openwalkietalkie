//! The hand-off itself (`deck_model::handoff`): bring a session's own app
//! forward, and never launch one.
//!
//! The order is the whole of the policy. The app must be running
//! (`NSRunningApplication`); then LaunchServices raises it (`/usr/bin/open
//! -b <id>`, openmicrokbd's "Return to task" path, the one a background
//! process is most likely to be allowed); only if that fails is AppKit
//! asked directly (`activateWithOptions`). `open -b` would launch an app
//! that is not running, and the running check a moment before it is what
//! keeps that window to milliseconds.
//!
//! No Apple Event, no Automation grant, no window or tab finer than the app.
//! macOS 14 made activation cooperative, and whether either path is honoured
//! from the daemon's launch agent has not been measured here (no test may
//! raise an app on this Mac): it is the owner's check on the installed
//! build. [`Raiser`] makes the order a one-line swap if `activateWithOptions`
//! has to go first.
use std::sync::Arc;
use std::time::Duration;

/// How long `/usr/bin/open` may take before it is killed and AppKit asked
/// instead.
pub const RAISE_BUDGET: Duration = Duration::from_secs(3);

/// What brings an app forward. The daemon's is [`MacRaiser`]; every test
/// uses `super::fakes::FakeRaiser`, because a real one would raise an app
/// on the machine the tests run on.
pub trait Raiser: Send + Sync + std::fmt::Debug {
    /// Whether an instance of `app`, a bundle identifier, is running.
    fn running(&self, app: &str) -> bool;
    /// Bring a running `app` forward, or say why not.
    fn raise(&self, app: &str) -> Result<(), String>;
    /// For the log.
    fn name(&self) -> &'static str;
}

/// What a hand-off came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Raised,
    /// The session named no app the daemon may raise, so nothing was
    /// asked of macOS: the core's own answer (`Core::hand_off`), never the
    /// worker's.
    NoApp,
    /// Not running, so nothing was launched.
    NotRunning,
    /// macOS would not bring it forward, and why.
    Refused(String),
}

impl Outcome {
    /// The word the log line carries.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Outcome::Raised => "raised",
            Outcome::NoApp => "no_app",
            Outcome::NotRunning => "not_running",
            Outcome::Refused(_) => "refused",
        }
    }

    /// What the deck is shown for a hand-off that did not happen; `None`
    /// for one that did.
    #[must_use]
    pub fn failure(&self) -> Option<deck_model::HandoffFailure> {
        match self {
            Outcome::Raised => None,
            Outcome::NoApp => Some(deck_model::HandoffFailure::NoApp),
            Outcome::NotRunning => Some(deck_model::HandoffFailure::NotRunning),
            Outcome::Refused(_) => Some(deck_model::HandoffFailure::Refused),
        }
    }
}

/// Raise `app` if it is running. Never launches it: `raise` is not called
/// at all for an app that is not.
#[must_use]
pub fn hand_off(raiser: &dyn Raiser, app: &str) -> Outcome {
    if !raiser.running(app) {
        return Outcome::NotRunning;
    }
    match raiser.raise(app) {
        Ok(()) => Outcome::Raised,
        Err(why) => Outcome::Refused(why),
    }
}

/// LaunchServices first, then AppKit.
#[cfg(target_os = "macos")]
#[derive(Debug, Default)]
pub struct MacRaiser;

#[cfg(target_os = "macos")]
impl MacRaiser {
    /// `f` over the running instances of `app`, inside an autorelease pool:
    /// the daemon's threads have none of their own.
    fn instances<T>(
        app: &str,
        f: impl Fn(&[objc2::rc::Retained<objc2_app_kit::NSRunningApplication>]) -> T,
    ) -> T {
        use objc2_app_kit::NSRunningApplication;
        use objc2_foundation::NSString;
        objc2::rc::autoreleasepool(|_| {
            let live: Vec<_> = NSRunningApplication::runningApplicationsWithBundleIdentifier(
                &NSString::from_str(app),
            )
            .iter()
            .filter(|instance| !instance.isTerminated())
            .collect();
            f(&live)
        })
    }
}

#[cfg(target_os = "macos")]
impl Raiser for MacRaiser {
    fn running(&self, app: &str) -> bool {
        Self::instances(app, |live| !live.is_empty())
    }

    fn raise(&self, app: &str) -> Result<(), String> {
        use objc2_app_kit::NSApplicationActivationOptions;
        if Self::instances(app, |live| live.iter().any(|i| i.isActive())) {
            return Ok(());
        }
        match open_b(app) {
            Ok(()) => return Ok(()),
            Err(why) => tracing::debug!(app, why, "open -b did not raise it; asking AppKit"),
        }
        let activated = Self::instances(app, |live| {
            live.first().is_some_and(|instance| {
                if instance.isHidden() {
                    let _ = instance.unhide();
                }
                instance.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows)
            })
        });
        if activated {
            Ok(())
        } else {
            Err("refused".into())
        }
    }

    fn name(&self) -> &'static str {
        "launchservices"
    }
}

/// `/usr/bin/open -b <app>`, with nothing on its standard streams, killed
/// after [`RAISE_BUDGET`]. `app` is checked again here, where it becomes an
/// argument of another program.
#[cfg(target_os = "macos")]
fn open_b(app: &str) -> Result<(), String> {
    use std::process::{Command, Stdio};
    if !crate::hook::app::valid_bundle_id(app) {
        return Err("not a bundle identifier".into());
    }
    let mut child = Command::new("/usr/bin/open")
        .args(["-b", app])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("open did not start: {e}"))?;
    let deadline = std::time::Instant::now() + RAISE_BUDGET;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("open {status}")),
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("open timed out".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("open: {e}")),
        }
    }
}

/// Windows: the app is a program's executable name, and its front-most
/// window is brought forward (`crate::winwin`). Never launches anything.
#[cfg(windows)]
#[derive(Debug, Default)]
pub struct WinRaiser;

#[cfg(windows)]
impl Raiser for WinRaiser {
    fn running(&self, app: &str) -> bool {
        crate::winwin::running(app)
    }

    fn raise(&self, app: &str) -> Result<(), String> {
        crate::winwin::raise(app)
    }

    fn name(&self) -> &'static str {
        "windows"
    }
}

/// No app is ever raised where there is no desktop to raise it on.
#[derive(Debug, Default)]
pub struct NoRaiser;

impl Raiser for NoRaiser {
    fn running(&self, _app: &str) -> bool {
        false
    }

    fn raise(&self, _app: &str) -> Result<(), String> {
        Err("no desktop on this platform".into())
    }

    fn name(&self) -> &'static str {
        "none"
    }
}

/// The daemon's raiser on this platform.
#[must_use]
pub fn default_raiser() -> Arc<dyn Raiser> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(MacRaiser)
    }
    #[cfg(windows)]
    {
        Arc::new(WinRaiser)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Arc::new(NoRaiser)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::fakes::FakeRaiser;

    #[test]
    fn a_hand_off_never_launches_an_app_that_is_not_running() {
        let fake = FakeRaiser::new();
        assert_eq!(hand_off(&fake, "dev.zed.Zed"), Outcome::NotRunning);
        assert!(fake.raised().is_empty(), "raise was never asked");
        assert_eq!(Outcome::NotRunning.word(), "not_running");
    }

    #[test]
    fn a_running_app_is_raised_once() {
        let fake = FakeRaiser::new().with_running("dev.zed.Zed");
        assert_eq!(hand_off(&fake, "dev.zed.Zed"), Outcome::Raised);
        assert_eq!(fake.raised(), vec!["dev.zed.Zed".to_string()]);
        assert_eq!(Outcome::Raised.word(), "raised");
    }

    #[test]
    fn a_refusal_is_reported_by_word() {
        let fake = FakeRaiser::new()
            .with_running("com.mitchellh.ghostty")
            .refusing("refused");
        let outcome = hand_off(&fake, "com.mitchellh.ghostty");
        assert_eq!(outcome, Outcome::Refused("refused".into()));
        assert_eq!(outcome.word(), "refused");
        assert_eq!(fake.raised(), vec!["com.mitchellh.ghostty".to_string()]);
    }

    /// Every outcome but a raise is a failure the deck is shown.
    #[test]
    fn only_a_raise_is_not_a_failure() {
        use deck_model::HandoffFailure;
        assert_eq!(Outcome::Raised.failure(), None);
        assert_eq!(Outcome::NoApp.failure(), Some(HandoffFailure::NoApp));
        assert_eq!(
            Outcome::NotRunning.failure(),
            Some(HandoffFailure::NotRunning)
        );
        assert_eq!(
            Outcome::Refused("refused".into()).failure(),
            Some(HandoffFailure::Refused)
        );
        assert_eq!(Outcome::NoApp.word(), "no_app");
    }

    /// Off macOS nothing is ever running, so nothing is ever raised.
    #[test]
    fn no_raiser_raises_nothing() {
        assert_eq!(hand_off(&NoRaiser, "dev.zed.Zed"), Outcome::NotRunning);
        assert!(NoRaiser.raise("dev.zed.Zed").is_err());
    }
}
