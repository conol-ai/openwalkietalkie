//! `OpenWalkieTalkie.app`'s entry point. Everything is in the library beside
//! it, so the crate's own tests can reach it without opening a window.
//!
//! On Windows it is a GUI program: a console-subsystem executable opens a
//! console window beside its own, and closing that console ends the app at
//! once, with nothing logged.
#![cfg_attr(windows, windows_subsystem = "windows")]
fn main() {
    // Windows: `--service` makes this the background service's host (no
    // window; it keeps `deckd run` going, as launchd does on a Mac).
    #[cfg(windows)]
    if openwalkietalkie_app::service_host::requested() {
        std::process::exit(openwalkietalkie_app::service_host::supervise());
    }
    // Windows: `--update-to <version>` updates this copy to that release, as
    // the Service page's Update does, without a window (a script's way).
    #[cfg(windows)]
    if let Some(version) = openwalkietalkie_app::win_update::requested() {
        std::process::exit(openwalkietalkie_app::win_update::run_from_command_line(&version));
    }
    if let Err(e) = openwalkietalkie_app::run() {
        // There is no window to say this in: the tokio bridge is what a
        // window would have talked to the daemon through.
        eprintln!("openwalkietalkie-app: {e}");
        std::process::exit(1);
    }
}
