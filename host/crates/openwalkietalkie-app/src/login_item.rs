//! The login item: `SMAppService.mainApp` (§10.2, contract §1 M2).
//!
//! Three calls and one rule. The rule is the measured one (M2): on a bare
//! binary `status` went 0 → `register()` **Ok** → `status` **1 (enabled)** →
//! `unregister()` → 0, **but in a fresh state the first `register()` returned
//! `Ok` while `status` was still 3 (`notFound`)**. So the return value is not
//! evidence, and nothing in this module reports success from it: every
//! mutation re-reads `status()` and answers with that. A toggle that said
//! "on" having registered nothing is the exact failure the re-read exists to
//! stop.
//!
//! The second rule is not about correctness but about whose machine this is.
//! A login item is a **change to the operator's Mac** that outlives the
//! process, so [`register`] is only ever called from an explicit click, never
//! from a screen merely being open, and the Welcome step draws the state it
//! reads rather than the state it would like.
//!
//! Everything testable is next door: [`crate::present::LoginItem`] is the
//! status vocabulary and its rule, and it is a plain `isize` away from
//! anything Objective-C. What is here is the four unsafe lines that ask
//! macOS.
use crate::present::LoginItem;

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
/// `Err` carries what macOS said when the call itself failed. An `Ok` whose
/// [`LoginItem::registered`] is false is the M2 case: the call returned
/// success and macOS has no record of it, which the caller shows as the
/// status and never as "on".
#[cfg(target_os = "macos")]
pub fn register() -> Result<LoginItem, String> {
    // SAFETY: as above; `registerAndReturnError` writes the app's own
    // launchd-managed login item and reports failure through the `Result`.
    let service = unsafe { objc2_service_management::SMAppService::mainAppService() };
    let called = unsafe { service.registerAndReturnError() };
    let status = LoginItem::from_code(unsafe { service.status() }.0);
    match called {
        Ok(()) => Ok(status),
        // Even a failure is reported beside the status: a `register` that
        // errored on an already-enabled item has changed nothing and the
        // status is what the operator needs to see.
        Err(e) => Err(format!("{e} (status is now {})", status.code())),
    }
}

/// Unregister it, then re-read. Same rule, same reason.
#[cfg(target_os = "macos")]
pub fn unregister() -> Result<LoginItem, String> {
    // SAFETY: as above.
    let service = unsafe { objc2_service_management::SMAppService::mainAppService() };
    let called = unsafe { service.unregisterAndReturnError() };
    let status = LoginItem::from_code(unsafe { service.status() }.0);
    match called {
        Ok(()) => Ok(status),
        Err(e) => Err(format!("{e} (status is now {})", status.code())),
    }
}

/// Everything above, for a build that is not macOS. There is no login item
/// to read, so the honest answer is the one status that claims nothing.
#[cfg(not(target_os = "macos"))]
pub fn status() -> LoginItem {
    LoginItem::Unknown(-1)
}

#[cfg(not(target_os = "macos"))]
pub fn register() -> Result<LoginItem, String> {
    Err("SMAppService is macOS only".into())
}

#[cfg(not(target_os = "macos"))]
pub fn unregister() -> Result<LoginItem, String> {
    Err("SMAppService is macOS only".into())
}

/// Register the daemon embedded in an installed app. This is separate from
/// launching the GUI at login; it never enables the GUI login item.
#[cfg(target_os = "macos")]
pub fn register_bundled_agent() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let Some(contents) = exe
        .parent()
        .filter(|p| p.file_name().is_some_and(|n| n == "MacOS"))
        .and_then(|p| p.parent())
    else {
        return Ok(());
    };
    let name = "ai.cohdl.openwalkietalkie.deckd.plist";
    if !contents.join("Library/LaunchAgents").join(name).is_file() {
        return Ok(());
    }
    let name = objc2_foundation::NSString::from_str(name);
    // SAFETY: a fixed plist inside our own application, with its helper
    // path anchored by BundleProgram. macOS retains the consent decision.
    let service =
        unsafe { objc2_service_management::SMAppService::agentServiceWithPlistName(&name) };
    let status = unsafe { service.status() }.0;
    // 2: waiting on the operator's approval in System Settings.
    if status == 2 {
        return Ok(());
    }
    // 1: registered — but a registration can outlive the copy of the app
    // that made it (moved, replaced, booted out: `deckd doctor`'s "the
    // registration outlived the bundle"), and then launchd has no job and
    // nothing ever starts the daemon again. Only a registration launchd
    // actually holds is left alone; a dangling one is made again from this
    // copy.
    if status == 1 {
        if agent_loaded() {
            return Ok(());
        }
        let _ = unsafe { service.unregisterAndReturnError() };
    }
    unsafe { service.registerAndReturnError() }.map_err(|e| e.to_string())
}

/// Whether launchd holds the daemon's job in this user's GUI domain.
#[cfg(target_os = "macos")]
fn agent_loaded() -> bool {
    let uid = unsafe { libc::getuid() };
    std::process::Command::new("/bin/launchctl")
        .args(["print", &format!("gui/{uid}/ai.cohdl.openwalkietalkie.deckd")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        // A launchctl that cannot run proves nothing: leave it registered.
        .map_or(true, |s| s.success())
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
        assert!(
            before.code() >= 0,
            "an unbuilt SMAppService answers a negative code and nothing here should"
        );
    }
}
