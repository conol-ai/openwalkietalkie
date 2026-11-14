//! The login items: `SMAppService` for the background service's launch agent
//! and for this app's own "open at login" (§10.2, contract §1 M2).
//!
//! Three rules. The first is the measured one (M2): on a bare binary
//! `status` went 0 → `register()` **Ok** → `status` **1 (enabled)** →
//! `unregister()` → 0, **but in a fresh state the first `register()` returned
//! `Ok` while `status` was still 3 (`notFound`)**. So the return value is not
//! evidence, and nothing in this module reports success from it: every
//! mutation re-reads the status and answers with that. A toggle that said
//! "on" having registered nothing is the exact failure the re-read exists to
//! stop.
//!
//! The second is about whose machine this is. A login item is a **change to
//! the operator's Mac** that outlives the process, so nothing here runs
//! because a screen is open: registering the service is first run's **Add
//! These** and the Service page's Start at login, both clicks, and the boot
//! only makes again a registration launchd lost
//! ([`repair_agent_if_registered`]). The window draws the status it reads,
//! never the one it would like.
//!
//! The third is about which copy of the app is asking. Only the installed
//! app, run for real ([`crate::app::Launch::installed`]), changes either item:
//! the agent's label and plist are the owner's whichever copy names them, so
//! a build outside the bundle, a `--shots` walk or a window on a test
//! service's `DECKD_DATA_DIR` would be registering, unregistering or
//! restarting his own service. In those runs every change here answers `Err`
//! before macOS is asked anything, and the Mac is left as it was. The one
//! exception is this binary's own login item under [`LIVE`], the opt-in half
//! of `tests/login_item.rs`, which registers a bare test binary for as long
//! as it takes to read the status back — never the service's, and never in
//! a `--shots` walk or a window on `DECKD_DATA_DIR` ([`may_change`]).
//!
//! Everything testable is next door: [`crate::present::LoginItem`] is the
//! status vocabulary and its rule, and it is a plain `isize` away from
//! anything Objective-C. What is here is the few unsafe lines that ask
//! macOS.
use crate::app::Launch;
use crate::present::LoginItem;

/// `OPENWALKIETALKIE_LOGIN_ITEM_LIVE=1`: the opt-in of `tests/login_item.rs`,
/// which registers this binary's own login item for as long as it takes to
/// read the status back. It never lets anything change the service's agent.
pub const LIVE: &str = "OPENWALKIETALKIE_LOGIN_ITEM_LIVE";

/// Whether a run may change a login item: the installed app may change both;
/// a run that opted in with [`LIVE`] (`live`) may change this binary's own
/// (`main_item`) and never the service's — and only as the test it is for,
/// never as a `--shots` walk or a window on a test service's
/// `DECKD_DATA_DIR`, which change nothing outside their window whatever
/// else is set.
pub const fn may_change(launch: Launch, main_item: bool, live: bool) -> bool {
    launch.installed() || (main_item && live && !launch.harness && !launch.data_dir)
}

/// Why this process may not change the item, if it may not.
#[cfg(target_os = "macos")]
fn refused(main_item: bool) -> Option<&'static str> {
    let launch = crate::app::launch();
    let live = std::env::var(LIVE).as_deref() == Ok("1");
    if may_change(launch, main_item, live) {
        None
    } else {
        launch.hands_off()
    }
}

/// A refusal, and the status the item kept.
#[cfg(target_os = "macos")]
fn unchanged(why: &str, status: LoginItem) -> String {
    format!("{why}; nothing changed (status is {})", status.code())
}

/// The status after a call, which is what is reported, with the call's own
/// failure beside it: a `register` that errored on an already-enabled item
/// has changed nothing, and the status is what the operator needs to see.
#[cfg(target_os = "macos")]
fn reread(
    service: &objc2_service_management::SMAppService,
    called: Result<(), objc2::rc::Retained<objc2_foundation::NSError>>,
) -> Result<LoginItem, String> {
    // SAFETY: `status` reads the service object and cannot fail.
    let status = LoginItem::from_code(unsafe { service.status() }.0);
    match called {
        Ok(()) => Ok(status),
        Err(e) => Err(format!("{e} (status is now {})", status.code())),
    }
}

// ---- this app's own login item ---------------------------------------------

/// What this app's login item is, as macOS reports it right now.
///
/// Never cached. `SMAppService` is the truth and the operator can change it
/// from System Settings while this window is open.
#[cfg(target_os = "macos")]
pub fn status() -> LoginItem {
    // SAFETY: `mainAppService` returns the process's own service object and
    // `status` reads it; neither takes an argument and neither can fail.
    // This is a main-thread-agnostic read — `SMAppService` is not an AppKit
    // class — but every caller here is on the UI thread anyway.
    let service = unsafe { objc2_service_management::SMAppService::mainAppService() };
    LoginItem::from_code(unsafe { service.status() }.0)
}

/// Register this app to open at login, then **re-read the status** and
/// report that (M2).
///
/// `Err` carries what macOS said when the call itself failed, or why this
/// run may not ask. An `Ok` whose [`LoginItem::registered`] is false is the
/// M2 case: the call returned success and macOS has no record of it, which
/// the caller shows as the status and never as "on".
#[cfg(target_os = "macos")]
pub fn register() -> Result<LoginItem, String> {
    if let Some(why) = refused(true) {
        return Err(unchanged(why, status()));
    }
    // SAFETY: as above; `registerAndReturnError` writes the app's own
    // launchd-managed login item and reports failure through the `Result`.
    let service = unsafe { objc2_service_management::SMAppService::mainAppService() };
    let called = unsafe { service.registerAndReturnError() };
    reread(&service, called)
}

/// Unregister it, then re-read. Same rules, same reasons.
#[cfg(target_os = "macos")]
pub fn unregister() -> Result<LoginItem, String> {
    if let Some(why) = refused(true) {
        return Err(unchanged(why, status()));
    }
    // SAFETY: as above.
    let service = unsafe { objc2_service_management::SMAppService::mainAppService() };
    let called = unsafe { service.unregisterAndReturnError() };
    reread(&service, called)
}

// ---- the background service's launch agent --------------------------------

/// The service's launch agent in this copy of the app, when this copy is the
/// bundle that carries one ([`crate::app::installed_copy`]). A build outside
/// the bundle has none, and so asks `SMAppService` nothing.
#[cfg(target_os = "macos")]
fn agent() -> Option<objc2::rc::Retained<objc2_service_management::SMAppService>> {
    if !crate::app::launch().bundled {
        return None;
    }
    let name = objc2_foundation::NSString::from_str(crate::app::DECKD_PLIST);
    // SAFETY: a fixed plist inside our own application, with its helper
    // path anchored by BundleProgram. macOS retains the consent decision.
    Some(unsafe { objc2_service_management::SMAppService::agentServiceWithPlistName(&name) })
}

/// The background service's launch agent, as macOS reports it right now.
/// Never cached, for the reason [`status`] is not.
///
/// A copy of the app that carries no agent has none to report: `NotFound`,
/// without asking.
#[cfg(target_os = "macos")]
pub fn agent_status() -> LoginItem {
    // SAFETY: `status` reads the service object and cannot fail.
    agent().map_or(LoginItem::NotFound, |service| {
        LoginItem::from_code(unsafe { service.status() }.0)
    })
}

/// Register the service's launch agent — first run's **Add These**, the
/// Service page's Start at login — then re-read the status and report that.
///
/// `RequiresApproval` is a success that is waiting for the owner in System
/// Settings › General › Login Items; the service starts once he allows it.
#[cfg(target_os = "macos")]
pub fn register_agent() -> Result<LoginItem, String> {
    if let Some(why) = refused(false) {
        return Err(unchanged(why, agent_status()));
    }
    let service = agent().ok_or(NO_AGENT)?;
    // SAFETY: as in `agent`; failure is reported through the `Result`.
    let called = unsafe { service.registerAndReturnError() };
    reread(&service, called)
}

/// Unregister the service's launch agent — the Service page's Start at login
/// turned off, and Uninstall — then re-read. launchd stops the service with
/// it: the deck stops working until it is registered again.
#[cfg(target_os = "macos")]
pub fn unregister_agent() -> Result<LoginItem, String> {
    if let Some(why) = refused(false) {
        return Err(unchanged(why, agent_status()));
    }
    let service = agent().ok_or(NO_AGENT)?;
    // SAFETY: as in `agent`.
    let called = unsafe { service.unregisterAndReturnError() };
    reread(&service, called)
}

/// Make again, from this copy, a registration launchd lost; touch nothing
/// else. The boot calls it.
///
/// A registration can outlive the copy of the app that made it (moved,
/// replaced, booted out: `deckd doctor`'s "the registration outlived the
/// bundle"), and then launchd has no job and nothing ever starts the service
/// again. Only that case is repaired: an enabled registration launchd does
/// not hold is unregistered and registered again from here. A service that
/// was never registered is first run's to set up, and one waiting for the
/// owner's approval is his to allow; both are left as they are, and the
/// status is the answer.
#[cfg(target_os = "macos")]
pub fn repair_agent_if_registered() -> Result<LoginItem, String> {
    if let Some(why) = refused(false) {
        return Err(unchanged(why, agent_status()));
    }
    let Some(service) = agent() else {
        return Ok(LoginItem::NotFound);
    };
    // SAFETY: as in `agent`.
    let status = LoginItem::from_code(unsafe { service.status() }.0);
    if status != LoginItem::Enabled || agent_loaded() {
        return Ok(status);
    }
    // SAFETY: as in `agent`. The unregister's own answer is not needed: the
    // register that follows and its re-read say how it ended.
    let _ = unsafe { service.unregisterAndReturnError() };
    let called = unsafe { service.registerAndReturnError() };
    reread(&service, called)
}

/// Why a run that may change the agent found none to change. Only a bundle
/// can be the installed app, so this is a copy that is neither.
#[cfg(target_os = "macos")]
const NO_AGENT: &str = "this copy of the app carries no launch agent";

/// Whether launchd holds the daemon's job in this user's GUI domain. A read:
/// `launchctl print` changes nothing.
#[cfg(target_os = "macos")]
fn agent_loaded() -> bool {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    std::process::Command::new("/bin/launchctl")
        .args(["print", &format!("gui/{uid}/{}", crate::app::DECKD_LABEL)])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        // A launchctl that cannot run proves nothing: leave it registered.
        .map_or(true, |s| s.success())
}

// ---- Windows: the per-user Run key ------------------------------------------
//
// Windows' equivalent of both login items is a value under
// `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, the list Settings ›
// Apps › Startup shows and switches. Switching one off there leaves the value
// and records the choice under `Explorer\StartupApproved\Run`; that reads as
// `RequiresApproval`, the owner's to turn back on, exactly as a macOS item
// waiting in Login Items. The same three rules hold: only the installed copy
// changes anything, every change answers with the status read back after
// it, and nothing is registered because a screen opened.
//
// The service's value runs this app as `--service` ([`crate::service_host`]):
// no window, it keeps `deckd run` going the way launchd's KeepAlive does.

#[cfg(windows)]
use crate::run_key;

/// The Run value of this app's own "open at login".
#[cfg(windows)]
const APP_VALUE: &str = "OpenWalkieTalkie";
/// The Run value of the background service.
#[cfg(windows)]
const SERVICE_VALUE: &str = "OpenWalkieTalkie Service";

/// What a Run value should hold for this copy: the app itself, or the app as
/// the service's host.
#[cfg(windows)]
fn run_command(service: bool) -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(if service {
        format!("\"{}\" {}", exe.display(), crate::service_host::FLAG)
    } else {
        format!("\"{}\"", exe.display())
    })
}

#[cfg(windows)]
fn refused(main_item: bool) -> Option<&'static str> {
    let launch = crate::app::launch();
    let live = std::env::var(LIVE).as_deref() == Ok("1");
    if may_change(launch, main_item, live) {
        None
    } else {
        launch.hands_off()
    }
}

#[cfg(windows)]
fn read_status(value: &str) -> LoginItem {
    match run_key::command(value) {
        None => LoginItem::NotRegistered,
        Some(_) if run_key::approved(value) => LoginItem::Enabled,
        Some(_) => LoginItem::RequiresApproval,
    }
}

/// This app's own "open at login", as Windows has it right now.
#[cfg(windows)]
pub fn status() -> LoginItem {
    read_status(APP_VALUE)
}

/// Open this app at sign-in, then re-read.
#[cfg(windows)]
pub fn register() -> Result<LoginItem, String> {
    if let Some(why) = refused(true) {
        return Err(format!("{why}; nothing changed (status is {})", status().code()));
    }
    let command = run_command(false).ok_or("this app's own path is unknown")?;
    run_key::set(APP_VALUE, &command).map(|()| status())
}

/// Stop opening this app at sign-in, then re-read.
#[cfg(windows)]
pub fn unregister() -> Result<LoginItem, String> {
    if let Some(why) = refused(true) {
        return Err(format!("{why}; nothing changed (status is {})", status().code()));
    }
    run_key::delete(APP_VALUE).map(|()| status())
}

/// The background service's start at sign-in. A copy that is not the
/// installed one carries no service to report: `NotFound`, without asking.
#[cfg(windows)]
pub fn agent_status() -> LoginItem {
    if !crate::app::launch().bundled {
        return LoginItem::NotFound;
    }
    read_status(SERVICE_VALUE)
}

/// Start the service at every sign-in, and now, as macOS's agent starts when
/// it is registered; then re-read.
#[cfg(windows)]
pub fn register_agent() -> Result<LoginItem, String> {
    if let Some(why) = refused(false) {
        return Err(format!("{why}; nothing changed (status is {})", agent_status().code()));
    }
    let command = run_command(true).ok_or("this app's own path is unknown")?;
    run_key::set(SERVICE_VALUE, &command)?;
    if let Err(why) = crate::service_host::start() {
        return Err(format!("start at sign-in is set, but the service did not start: {why}"));
    }
    Ok(agent_status())
}

/// Stop starting the service at sign-in, and stop it now (the deck stops
/// working until it is started again), as unregistering macOS's agent does;
/// then re-read.
#[cfg(windows)]
pub fn unregister_agent() -> Result<LoginItem, String> {
    if let Some(why) = refused(false) {
        return Err(format!("{why}; nothing changed (status is {})", agent_status().code()));
    }
    run_key::delete(SERVICE_VALUE)?;
    crate::service_host::stop();
    Ok(agent_status())
}

/// Point a registration this copy no longer matches (the app was moved or
/// replaced since it was made) at this copy; touch nothing else.
#[cfg(windows)]
pub fn repair_agent_if_registered() -> Result<LoginItem, String> {
    if let Some(why) = refused(false) {
        return Err(format!("{why}; nothing changed (status is {})", agent_status().code()));
    }
    let Some(registered) = run_key::command(SERVICE_VALUE) else {
        return Ok(LoginItem::NotRegistered);
    };
    let command = run_command(true).ok_or("this app's own path is unknown")?;
    if !registered.eq_ignore_ascii_case(&command) {
        run_key::set(SERVICE_VALUE, &command)?;
    }
    Ok(agent_status())
}

// ---- everything above, elsewhere --------------------------------------------
//
// There is no login item to read or to write, so the honest answer is the
// one status that claims nothing, and a refusal for every change. The
// service's own setup there is `host/omarchy/install.sh`'s, not this app's.

#[cfg(not(any(target_os = "macos", windows)))]
const MACOS_ONLY: &str = "SMAppService is macOS only";

#[cfg(not(any(target_os = "macos", windows)))]
pub fn status() -> LoginItem {
    LoginItem::Unknown(-1)
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn register() -> Result<LoginItem, String> {
    Err(MACOS_ONLY.into())
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn unregister() -> Result<LoginItem, String> {
    Err(MACOS_ONLY.into())
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn agent_status() -> LoginItem {
    LoginItem::Unknown(-1)
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn register_agent() -> Result<LoginItem, String> {
    Err(MACOS_ONLY.into())
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn unregister_agent() -> Result<LoginItem, String> {
    Err(MACOS_ONLY.into())
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn repair_agent_if_registered() -> Result<LoginItem, String> {
    Err(MACOS_ONLY.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_the_login_item_changes_nothing_on_this_machine() {
        // The read is safe to run in any suite, on any machine, as often as
        // it likes: it is a status query and registers nothing. Whatever it
        // answers, it must answer one of the statuses this build can name
        // — and the one thing the test asserts is the one thing a test may
        // assert about the operator's own Mac: that asking did not change
        // it.
        let before = status();
        let again = status();
        assert_eq!(before, again, "reading twice must give the same answer");
        #[cfg(target_os = "macos")]
        assert!(
            before.code() >= 0,
            "an unbuilt SMAppService answers a negative code and nothing here should"
        );
    }

    #[test]
    fn only_the_installed_app_changes_a_login_item_and_a_test_only_its_own_by_opting_in() {
        let installed = Launch {
            bundled: true,
            ..Launch::default()
        };
        for main_item in [false, true] {
            for live in [false, true] {
                assert!(may_change(installed, main_item, live));
            }
        }
        // A build or a test binary: this binary's own item by the opt-in
        // alone, the service's never.
        let build = Launch::default();
        assert!(!may_change(build, false, false));
        assert!(!may_change(build, false, true));
        assert!(!may_change(build, true, false));
        assert!(may_change(build, true, true));
        // A harness walk and a window on a test service change nothing
        // outside their window, opted in or not, bundled or not.
        for bundled in [false, true] {
            for launch in [
                Launch {
                    bundled,
                    harness: true,
                    ..Launch::default()
                },
                Launch {
                    bundled,
                    data_dir: true,
                    ..Launch::default()
                },
            ] {
                for main_item in [false, true] {
                    for live in [false, true] {
                        assert!(!may_change(launch, main_item, live), "{launch:?}");
                    }
                }
            }
        }
        assert_eq!(LIVE, "OPENWALKIETALKIE_LOGIN_ITEM_LIVE");
    }

    #[test]
    fn a_test_binary_carries_no_agent_and_every_change_to_it_is_refused() {
        // This process never booted, so it is a build outside the bundle:
        // the agent's status is answered without asking macOS, and each
        // change is refused before macOS is asked anything — with or without
        // the opt-in, which is never the service's.
        assert!(!crate::app::launch().installed());
        let before = agent_status();
        #[cfg(target_os = "macos")]
        assert_eq!(before, LoginItem::NotFound);
        let changes: [fn() -> Result<LoginItem, String>; 3] =
            [register_agent, unregister_agent, repair_agent_if_registered];
        for change in changes {
            let answer = change();
            assert!(answer.is_err(), "{answer:?}");
            assert_eq!(agent_status(), before);
        }
    }
}
