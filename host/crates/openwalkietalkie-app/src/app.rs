//! The bootstrap (§8.1): the tokio bridge, the bus, the `ApiClient`, the
//! menus and the one window.
//!
//! The order matters. The bridge and the client start **before** the platform
//! loop, so the socket is already being dialled while the window is being
//! built and the first paint has a snapshot to draw; the bridge then moves
//! into a `gpui` [`Global`] so it lives exactly as long as the app and its
//! runtime thread is torn down on quit and not before.
//!
//! What is deliberately *not* here: nothing in this module, or anywhere above
//! `deck-gui-core`, can name a tokio `Handle`. [`TokioBridge`] hands out a
//! spawn and keeps its handle private, so the UI thread has nothing to call
//! `block_on` on. Socket I/O happens on the bridge; the replies are awaited
//! with `cx.spawn` on gpui's own foreground executor.
use crate::assets::Assets;
use crate::shell::{initial_screen, Shell};
use crate::shots::Shots;
use crate::style::{self, Palette};
use deck_gui_core::{api_client, events, ApiClient, EventReceiver, HostState, TokioBridge};
use gpui::{
    point, px, size, App, AppContext, Application, Bounds, Global, KeyBinding, Menu, MenuItem,
    Result, TitlebarOptions, WindowBackgroundAppearance, WindowBounds, WindowOptions,
};
use gpui_component::Root;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

gpui::actions!(openwalkietalkie, [Quit]);

/// `app_id` (§8.1) — and the bundle identifier TCC keys every grant to
/// (§10.1), so it is not a string to vary between builds.
pub const APP_ID: &str = "ai.cohdl.openwalkietalkie";

/// The launchd label of the daemon the window's Start button kicks (§10.1).
pub const DECKD_LABEL: &str = "ai.cohdl.openwalkietalkie.deckd";

/// The launchd log `deckd run` dup2s its own fds 1/2 onto (§9), which is the
/// one file that says why a service that will not start will not start.
pub const LAUNCHD_LOG: &str = "deckd.launchd.log";

/// The window: 1120×720, never smaller than 880×560. §8.1's 880×560 is
/// the floor now rather than the default: the sidebar carries icons and the
/// connection, and the Deck's roster and detail need the room beside it.
pub const WINDOW_WIDTH: f32 = 1120.;
pub const WINDOW_HEIGHT: f32 = 720.;
pub const MIN_WIDTH: f32 = 880.;
pub const MIN_HEIGHT: f32 = 560.;

/// The tokio runtime, parked in a global so it outlives every view and dies
/// with the app. §8.1 names the shape (`struct Tokio(Handle)`); this one
/// holds the [`TokioBridge`] instead, because the bridge is what owns the
/// runtime thread — and because a global that cannot produce a `Handle`
/// cannot be `block_on`ed by a view that finds it.
pub struct Tokio(pub TokioBridge);

impl Global for Tokio {}

/// Start the runtime, the bus and the client, then open the window.
///
/// `Err` only when the runtime thread could not be created, which on this
/// platform means the process is out of threads — there is no window to
/// report that in, so `main` prints it and exits non-zero.
pub fn run() -> std::io::Result<()> {
    // `--shots DIR`: draw every screen once, save each to DIR as a PNG and
    // quit. The window and the daemon are the real ones.
    let shots = Shots::from_args(std::env::args().skip(1));
    let bridge = TokioBridge::start()?;
    let (sender, receiver) = deck_gui_core::bus();
    // The app-wide bus, so a `state.get` reply can be posted from wherever
    // the view awaited it without threading a sender through every closure.
    events::install(sender.clone());
    let state = if shots.as_ref().is_some_and(Shots::synthetic_only) {
        HostState::detached()
    } else {
        let client = ApiClient::start(
            &bridge,
            sender,
            api_client::config(api_client::default_socket()),
        );
        HostState::new(client)
    };
    let application = Application::new().with_assets(Assets);
    // Clicking the dock icon of an app whose window was closed (and so
    // hidden, see `open`) has to bring it back, or hide-on-close is a
    // one-way door. This lives on the `Application` and not on `App`, so it
    // is registered before the platform loop rather than inside it.
    application.on_reopen(show);
    application.run(move |cx: &mut App| boot(bridge, state, receiver, shots, cx));
    Ok(())
}

/// Everything that happens once the platform loop is up.
fn boot(
    bridge: TokioBridge,
    state: HostState,
    receiver: EventReceiver,
    shots: Option<Shots>,
    cx: &mut App,
) {
    gpui_component::init(cx);
    let preview = shots.as_ref().is_some_and(Shots::synthetic_only);
    #[cfg(target_os = "macos")]
    if !preview {
        if let Err(error) = crate::login_item::register_bundled_agent() {
            eprintln!("openwalkietalkie-app: background service registration: {error}");
        }
    }
    style::register_fonts(cx);
    // Pinned before the window exists, so it opens in the right appearance
    // and the traffic lights match the palette.
    #[cfg(target_os = "macos")]
    if let Some(dark) = style::pinned_appearance() {
        crate::macos::pin_appearance(dark);
    }
    if let Some(shots) = shots {
        cx.set_global(shots);
    }
    cx.set_global(Tokio(bridge));
    cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
    cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
    let language = deck_gui_core::i18n::Language::from_id(state.mirror().settings.language.min(255) as u8);
    apply_menu_language(language, cx);
    // The status item, before the window: §8.2's tray-only mode means the
    // tray has to be there whether or not a window ever opens, and a menu
    // bar that refused it is not fatal — the window works exactly as it did.
    if let Some(tray) = crate::tray::install_localized(language) {
        cx.set_global(tray);
    }
    if let Err(e) = open(state, receiver, cx) {
        eprintln!("openwalkietalkie-app: could not open the window: {e}");
        cx.quit();
        return;
    }
    cx.activate(true);
}

/// Keep the app-owned menu item in the same language as the shared snapshot.
pub fn apply_menu_language(language: deck_gui_core::i18n::Language, cx: &mut App) {
    cx.set_menus(vec![Menu {
        name: "OpenWalkieTalkie".into(),
        items: vec![MenuItem::action(deck_gui_core::TrayCommand::Quit.label_localized(language), Quit)],
    }]);
}

fn open(state: HostState, receiver: EventReceiver, cx: &mut App) -> Result<()> {
    // The glass needs something blurred behind it; without the blur it is
    // painted opaque (see `style::Palette::frosted`).
    let frosted = cfg!(target_os = "macos") && style::glass_allowed();
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
            cx,
        ))),
        window_min_size: Some(size(px(MIN_WIDTH), px(MIN_HEIGHT))),
        // The content runs under a transparent title bar; the traffic lights
        // sit at (14, 14), centred in the shell's 38 px titlebar band.
        titlebar: Some(TitlebarOptions {
            title: None,
            appears_transparent: true,
            traffic_light_position: Some(point(px(14.), px(14.))),
        }),
        window_background: if frosted {
            WindowBackgroundAppearance::Blurred
        } else {
            WindowBackgroundAppearance::Opaque
        },
        app_id: Some(APP_ID.to_string()),
        ..Default::default()
    };
    cx.open_window(options, |window, cx| {
        #[cfg(target_os = "macos")]
        let frosted = frosted && crate::macos::use_under_window_material(window);
        if !frosted {
            window.set_background_appearance(WindowBackgroundAppearance::Opaque);
        }
        style::install(Palette::for_appearance(window.appearance(), frosted), cx);
        // Closing the window hides the app; the daemon keeps running and the
        // roster on the deck does not change (§8.2's tray-only mode).
        window.on_window_should_close(cx, |_, cx| {
            cx.hide();
            false
        });
        let shell = cx.new(|cx| Shell::new(state, receiver, initial_screen(), window, cx));
        if cx.has_global::<Shots>() {
            crate::shots::start(shell.clone(), window, cx);
        }
        // The window's first layer must be a `gpui_component::Root`: it owns
        // the notification state the screens raise.
        cx.new(|cx| Root::new(shell, window, cx))
    })?;
    Ok(())
}

/// Bring the window back (the dock icon, a `ShowWindow` effect).
pub fn show(cx: &mut App) {
    cx.activate(true);
    for window in cx.windows() {
        window
            .update(cx, |_, window, _| window.activate_window())
            .ok();
    }
}

/// The launchd log path, honouring `DECKD_DATA_DIR` exactly as `deckd` does
/// (§9): a gate or a test run on a throwaway data dir must point the window
/// at that run's log and not at the operator's.
pub fn launchd_log() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    log_path(std::env::var_os("DECKD_DATA_DIR"), &home)
}

/// [`launchd_log`] without the environment, so it can be tested without
/// writing to a process-global.
pub fn log_path(data_dir: Option<OsString>, home: &Path) -> PathBuf {
    match data_dir {
        Some(dir) => PathBuf::from(dir).join("logs").join(LAUNCHD_LOG),
        None => home.join("Library/Logs/OpenWalkieTalkie").join(LAUNCHD_LOG),
    }
}

/// The `launchctl` invocation the Start button runs. `kickstart -k` starts a
/// loaded agent and restarts a wedged one, which is the two things the yellow
/// bar can mean.
///
/// Registering the agent in the first place is `SMAppService` and the Welcome
/// sheet's job (§10.2); this button is for the agent that is installed and
/// not running.
pub fn kickstart_argv() -> Vec<String> {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    vec![
        "kickstart".to_string(),
        "-k".to_string(),
        format!("gui/{uid}/{DECKD_LABEL}"),
    ]
}

/// What `launchctl print` is asked about: this user's own agent, by label.
/// The Service screen reads its `state`/`pid` (§8.2); nothing here can
/// change it.
pub fn print_argv() -> Vec<String> {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    vec!["print".to_string(), format!("gui/{uid}/{DECKD_LABEL}")]
}

/// Run [`kickstart_argv`] off the UI thread and answer on the returned
/// channel — which is what the [`Tokio`] global is for. The `waitpid` goes
/// to `spawn_blocking` and not to a runtime worker: the workspace's tokio
/// pin carries no `process` feature, so this is a blocking `std::process`
/// call and parking one of the two workers on it would stall the socket.
pub fn start_service(bridge: &TokioBridge) -> Receiver {
    let (tx, rx) = async_channel::bounded(1);
    bridge.spawn(async move {
        let answer = tokio::task::spawn_blocking(kickstart)
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        let _ = tx.send(answer).await;
    });
    rx
}

/// [`print_argv`] off the UI thread, answering with whatever `launchctl`
/// wrote — stdout and stderr alike, because "Could not find service" is on
/// the second one and is the answer the screen most needs.
///
/// A `launchctl` that cannot be spawned at all answers with the error text,
/// which `present::launchd_state` reads as `Unknown`: a screen that said
/// "not registered" because this app could not run a binary would be
/// reporting its own failure as the operator's.
pub fn launchd_print(bridge: &TokioBridge) -> async_channel::Receiver<String> {
    let (tx, rx) = async_channel::bounded(1);
    bridge.spawn(async move {
        let answer = tokio::task::spawn_blocking(print_launchd)
            .await
            .unwrap_or_else(|e| e.to_string());
        let _ = tx.send(answer).await;
    });
    rx
}

/// Where a [`start_service`] answer lands.
pub type Receiver = async_channel::Receiver<std::result::Result<(), String>>;

/// The blocking half: spawn `launchctl` and read its verdict.
fn kickstart() -> std::result::Result<(), String> {
    match std::process::Command::new("launchctl")
        .args(kickstart_argv())
        .output()
    {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(if why.is_empty() {
                format!("launchctl exited {}", out.status)
            } else {
                why
            })
        }
        Err(e) => Err(e.to_string()),
    }
}

/// The blocking half of [`launchd_print`]. Both streams, joined: launchd
/// prints the domain dump on stdout and the refusal on stderr.
fn print_launchd() -> String {
    match std::process::Command::new("launchctl")
        .args(print_argv())
        .output()
    {
        Ok(out) => {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            text.push('\n');
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            text
        }
        Err(e) => e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_is_the_designs_size_and_can_never_be_squeezed_past_its_minimum() {
        assert_eq!((WINDOW_WIDTH, WINDOW_HEIGHT), (1120., 720.));
        // §8.1's own size is the floor.
        assert_eq!((MIN_WIDTH, MIN_HEIGHT), (880., 560.));
        const { assert!(MIN_WIDTH < WINDOW_WIDTH && MIN_HEIGHT < WINDOW_HEIGHT) };
        assert_eq!(APP_ID, "ai.cohdl.openwalkietalkie");
    }

    #[test]
    fn the_start_button_kickstarts_this_users_own_agent_by_label() {
        let argv = kickstart_argv();
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        assert_eq!(argv[0], "kickstart");
        assert_eq!(
            argv[1], "-k",
            "a wedged agent has to be restarted, not skipped"
        );
        assert_eq!(
            argv[2],
            format!("gui/{uid}/ai.cohdl.openwalkietalkie.deckd")
        );
        assert!(
            !argv[2].contains("system/"),
            "the daemon is a per-user agent; nothing here asks for root"
        );
    }

    #[test]
    fn the_service_screen_asks_launchd_about_this_users_own_agent_and_nothing_else() {
        let argv = print_argv();
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        assert_eq!(argv, vec!["print", &format!("gui/{uid}/{DECKD_LABEL}")]);
        assert!(
            !argv.iter().any(|a| a.contains("system/")),
            "the daemon is a per-user agent; nothing here asks for root"
        );
        // `print` reads. Anything that could change the agent belongs on a
        // button the operator pressed, not on a screen that is merely open.
        assert_eq!(argv[0], "print");
    }

    #[test]
    fn the_launchd_log_follows_the_data_dir_the_daemon_was_given() {
        let home = Path::new("/Users/nobody");
        assert_eq!(
            log_path(Some("/tmp/owt-gate".into()), home),
            PathBuf::from("/tmp/owt-gate/logs/deckd.launchd.log")
        );
        assert_eq!(
            log_path(None, home),
            PathBuf::from("/Users/nobody/Library/Logs/OpenWalkieTalkie/deckd.launchd.log")
        );
    }
}
