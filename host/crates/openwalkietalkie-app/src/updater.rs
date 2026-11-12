//! Thin main-thread wrapper around Sparkle's standard macOS updater UI.
//!
//! Ordinary `cargo run` and test builds do not link a third-party framework.
//! The release packaging script opts into the bridge with a pinned embedded
//! `Sparkle.framework`; ad-hoc packages additionally disable it in Info.plist.

#[derive(Debug, Default)]
pub struct MacOsUpdater {
    signed_release: bool,
    started: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateCheck {
    Started,
    Busy,
}

impl MacOsUpdater {
    /// Start Sparkle when this is a signed release bundle with a configured
    /// feed. Must be called on the macOS application main thread.
    pub fn new() -> Self {
        Self::starting(true, platform::is_enabled, platform::start)
    }

    /// The updater for this run: Sparkle's only in the installed app run
    /// for real (`installed`, [`crate::app::Launch::installed`]). Sparkle
    /// checks the network at launch, writes its keys into the app's
    /// defaults and can install an update by itself, so a `--shots` walk, a
    /// window on a test service's `DECKD_DATA_DIR` or a build outside the
    /// bundle never starts it — not even in a signed release bundle — and
    /// says updates are unavailable instead.
    pub fn for_run(installed: bool) -> Self {
        Self::starting(installed, platform::is_enabled, platform::start)
    }

    /// [`MacOsUpdater::for_run`], with the platform's two calls passed in:
    /// `enabled` is asked only for the installed app, and `start` only for
    /// a signed release.
    fn starting(
        installed: bool,
        enabled: impl FnOnce() -> bool,
        start: impl FnOnce() -> bool,
    ) -> Self {
        if !installed {
            return Self::default();
        }
        let signed_release = enabled();
        Self {
            signed_release,
            started: signed_release && start(),
        }
    }

    /// Whether this bundle is configured to accept only the signed Sparkle
    /// feed. This deliberately stays true if controller initialization fails,
    /// so a release build can never fall back to the unsigned JSON manifest.
    pub fn uses_signed_updates(&self) -> bool {
        self.signed_release
    }

    pub fn can_check_for_updates(&self) -> bool {
        self.started && platform::can_check_for_updates()
    }

    pub fn session_in_progress(&self) -> bool {
        self.started && platform::session_in_progress()
    }

    pub fn check_for_updates_localized(&self, language: deck_gui_core::i18n::Language) -> Result<UpdateCheck, &'static str> {
        self.check_for_updates().map_err(|error| language.text(error))
    }

    /// Present Sparkle's standard check/download/install/relaunch flow.
    pub fn check_for_updates(&self) -> Result<UpdateCheck, &'static str> {
        if !self.signed_release {
            return Err("automatic updates are unavailable in this build");
        }
        if !self.started {
            return Err("the signed updater could not initialize");
        }
        if !platform::can_check_for_updates() {
            return Ok(UpdateCheck::Busy);
        }
        if platform::check_for_updates() {
            Ok(UpdateCheck::Started)
        } else {
            Err("the updater could not start an update check")
        }
    }
}

#[cfg(all(target_os = "macos", owt_sparkle))]
mod platform {
    unsafe extern "C" {
        fn owt_sparkle_is_enabled() -> bool;
        fn owt_sparkle_start() -> bool;
        fn owt_sparkle_can_check_for_updates() -> bool;
        fn owt_sparkle_session_in_progress() -> bool;
        fn owt_sparkle_check_for_updates() -> bool;
    }

    pub fn is_enabled() -> bool {
        // SAFETY: the owner constructs this wrapper from GPUI's main thread.
        unsafe { owt_sparkle_is_enabled() }
    }

    pub fn start() -> bool {
        // SAFETY: the owner constructs this wrapper from GPUI's main thread.
        unsafe { owt_sparkle_start() }
    }

    pub fn can_check_for_updates() -> bool {
        // SAFETY: callers are GPUI listeners running on the main thread.
        unsafe { owt_sparkle_can_check_for_updates() }
    }

    pub fn session_in_progress() -> bool {
        // SAFETY: callers are GPUI listeners running on the main thread.
        unsafe { owt_sparkle_session_in_progress() }
    }

    pub fn check_for_updates() -> bool {
        // SAFETY: callers are GPUI listeners running on the main thread.
        unsafe { owt_sparkle_check_for_updates() }
    }
}

#[cfg(not(all(target_os = "macos", owt_sparkle)))]
mod platform {
    pub fn is_enabled() -> bool {
        false
    }

    pub fn start() -> bool {
        false
    }

    pub fn can_check_for_updates() -> bool {
        false
    }

    pub fn session_in_progress() -> bool {
        false
    }

    pub fn check_for_updates() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_builds_have_an_explicit_manual_update_fallback() {
        let updater = MacOsUpdater::new();
        assert!(!updater.uses_signed_updates());
        assert!(!updater.can_check_for_updates());
        assert!(!updater.session_in_progress());
        assert_eq!(
            updater.check_for_updates(),
            Err("automatic updates are unavailable in this build")
        );
    }

    #[test]
    fn sparkle_is_asked_nothing_and_never_started_outside_the_installed_app() {
        // A harness walk, a window on a test service, a build: the platform
        // is not even asked whether this is a signed release.
        let elsewhere = MacOsUpdater::starting(
            false,
            || panic!("asked whether Sparkle is enabled"),
            || panic!("started Sparkle"),
        );
        assert!(!elsewhere.uses_signed_updates() && !elsewhere.started);
        assert_eq!(
            elsewhere.check_for_updates(),
            Err("automatic updates are unavailable in this build")
        );
        assert!(!MacOsUpdater::for_run(false).uses_signed_updates());
        // The installed app: a signed release starts it; an unsigned one
        // is asked, and starts nothing.
        let signed = MacOsUpdater::starting(true, || true, || true);
        assert!(signed.signed_release && signed.started);
        let failed = MacOsUpdater::starting(true, || true, || false);
        assert!(failed.uses_signed_updates() && !failed.started);
        let unsigned =
            MacOsUpdater::starting(true, || false, || panic!("started an unsigned release"));
        assert!(!unsigned.signed_release && !unsigned.started);
    }
}
