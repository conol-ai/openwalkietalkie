//! The bootstrap (§8.1): the tokio bridge, the bus, the `ApiClient`, the
//! menus and the one window — and the facts about this run that decide what
//! the app may do outside that window.
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
//!
//! **Only the installed app, run for real, reaches past its window**
//! ([`Launch`]). It alone registers, repairs or starts the background
//! service, has the service look for deck firmware by itself, keeps its
//! preferences in the Mac's own store and opens on first run by itself. A
//! build outside the bundle, a `--shots` walk and a window on a test
//! service's `DECKD_DATA_DIR` do none of it: the service's launch agent and
//! its label are the owner's, whichever copy of the app asks.
//!
//! **Nothing is installed by opening the app.** The boot no longer registers
//! the service: it only makes again a registration launchd lost
//! ([`crate::login_item::repair_agent_if_registered`]). Registering it is
//! first run's **Add These**, after a deck has been seen and the owner has
//! agreed ([`first_run_needed`]).
use crate::assets::Assets;
use crate::prefs::Prefs;
use crate::present::LoginItem;
use crate::shell::{initial_screen, Screen, Shell};
use crate::shots::Shots;
use crate::style;
use deck_gui_core::i18n::Language;
use deck_gui_core::{
    api_client, events, ApiClient, EventReceiver, HostState, MenuCommand, TokioBridge, UiAction,
};
use gpui::{
    point, px, size, Action, App, AppContext, Application, Bounds, Global, KeyBinding, Menu,
    MenuItem, OsAction, Pixels, Point, Result, TitlebarOptions, WindowBackgroundAppearance,
    WindowBounds, WindowOptions,
};
use gpui_component::{input, Root};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

gpui::actions!(
    openwalkietalkie,
    [
        /// **Quit OpenWalkieTalkie** (⌘Q).
        Quit,
        /// View ▸ **Deck** (⌘1).
        ShowDeck,
        /// View ▸ **Faces & Keys** (⌘2).
        ShowFacesKeys,
        /// View ▸ **Integrations** (⌘3).
        ShowIntegrations,
        /// View ▸ **Voice** (⌘4).
        ShowVoice,
        /// View ▸ **Service** (⌘5).
        ShowService,
        /// Window ▸ **Minimize** (⌘M).
        Minimize,
        /// Window ▸ **Zoom**.
        Zoom,
        /// Window ▸ **Close** (⌘W), which is what the window's close button
        /// does ([`close_window`]).
        CloseWindow,
    ]
);

/// `app_id` (§8.1) — and the bundle identifier TCC keys every grant to
/// (§10.1), so it is not a string to vary between builds.
pub const APP_ID: &str = "ai.cohdl.openwalkietalkie";

/// The launchd label of the daemon the window's Start button kicks (§10.1).
pub const DECKD_LABEL: &str = "ai.cohdl.openwalkietalkie.deckd";

/// The service's launch agent in the installed app, under
/// `Contents/Library/LaunchAgents/` (`packaging/macos/`): the plist
/// `SMAppService` registers the agent from.
pub const DECKD_PLIST: &str = "ai.cohdl.openwalkietalkie.deckd.plist";

/// `OPENWALKIETALKIE_FIRST_RUN`: `1` opens on first run whatever else holds,
/// `0` never does. Any other value is as if it were not set.
pub const FIRST_RUN_ENV: &str = "OPENWALKIETALKIE_FIRST_RUN";

/// The launchd log `deckd run` dup2s its own fds 1/2 onto (§9), which is the
/// one file that says why a service that will not start will not start.
pub const LAUNCHD_LOG: &str = "deckd.launchd.log";

/// The window opens at the design's 1040×780: the 212 px sidebar and the
/// main column, whose 764 px of content sit inside 32 px sides.
pub const WINDOW_WIDTH: f32 = 1040.;
pub const WINDOW_HEIGHT: f32 = 780.;
/// It can be made taller or wider, never narrower than the sidebar and a
/// whole content column, and never shorter than 600 (the column scrolls,
/// the sidebar does not, and its foot needs the room under the five pages).
pub const MIN_WIDTH: f32 = 1040.;
pub const MIN_HEIGHT: f32 = 600.;

/// Where the traffic lights are put: the close button's frame, from the
/// window's top-left corner. gpui moves the three buttons there and keeps
/// the spacing AppKit gives them.
///
/// The design centres them at (24, 26), (44, 26) and (64, 26), on the
/// sidebar's first row. AppKit draws them as 14 px circles, each centred 7 px
/// in and 7 px down from its frame's corner, 23 px apart (measured on the
/// harness's captures, macOS 27, 2026-11-11). So this puts the close button's
/// centre on the design's — measured at (24.0, 26.0), the circle whole, in
/// both appearances — and the other two 3 and 6 px right of theirs, which
/// only AppKit's own spacing could change.
pub const TRAFFIC_LIGHTS: (f32, f32) = (17., 19.);

/// [`TRAFFIC_LIGHTS`] as gpui takes it.
fn traffic_lights() -> Point<Pixels> {
    point(px(TRAFFIC_LIGHTS.0), px(TRAFFIC_LIGHTS.1))
}

/// The tokio runtime, parked in a global so it outlives every view and dies
/// with the app. §8.1 names the shape (`struct Tokio(Handle)`); this one
/// holds the [`TokioBridge`] instead, because the bridge is what owns the
/// runtime thread — and because a global that cannot produce a `Handle`
/// cannot be `block_on`ed by a view that finds it.
pub struct Tokio(pub TokioBridge);

impl Global for Tokio {}

/// How this copy of the app was started, which decides whether it may do
/// anything outside its own window ([`Launch::installed`]).
///
/// Each fact is one the owner's installed app never has but a developer's,
/// a test's or the harness's run does, and each alone is enough to keep the
/// run's hands off: the label it would kick, the agent it would register and
/// the firmware check it would set off are the owner's own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Launch {
    /// The executable is the installed app's ([`installed_copy`]): on macOS
    /// it sits in the bundle that carries the service's launch agent.
    pub bundled: bool,
    /// `--shots DIR`: the harness walks the window through its scenes.
    pub harness: bool,
    /// `DECKD_DATA_DIR` is set: the window talks to a service on a data
    /// directory of its own, a test's.
    pub data_dir: bool,
}

impl Launch {
    /// Read off this process: its executable and its environment. `harness`
    /// is whether it was given `--shots`.
    pub fn detect(harness: bool) -> Launch {
        Launch {
            bundled: std::env::current_exe()
                .is_ok_and(|exe| installed_copy(&exe, cfg!(target_os = "macos"))),
            harness,
            data_dir: std::env::var_os("DECKD_DATA_DIR").is_some(),
        }
    }

    /// Why this run changes nothing outside its own window, or `None` for the
    /// installed app run for real, the one run that may.
    pub const fn hands_off(self) -> Option<&'static str> {
        if self.harness {
            Some("a --shots run changes nothing outside its own window")
        } else if self.data_dir {
            Some("a window on DECKD_DATA_DIR changes nothing outside its own window")
        } else if !self.bundled {
            Some("only the installed app changes anything outside its own window")
        } else {
            None
        }
    }

    /// The installed app, run for real: not a build outside the bundle, not
    /// a harness walk, not a window on a test service's data directory.
    pub const fn installed(self) -> bool {
        self.hands_off().is_none()
    }

    /// The call that leaves for the service in place of `action`.
    ///
    /// `firmware.check` has the service open the deck over USB and fetch the
    /// release manifest from the network. Only the installed app, run for
    /// real, has it do that; in any other run a check the owner asks for
    /// goes as the `firmware.status` read the check would have ended in,
    /// whose reply has the same shape. Everything else goes as it was asked
    /// for. (The service's own looks are its own: `--no-device`, which a
    /// test service runs with, never starts one.)
    pub fn outgoing(self, action: UiAction) -> UiAction {
        match action {
            UiAction::FirmwareCheck { .. } if !self.installed() => UiAction::FirmwareStatus,
            action => action,
        }
    }
}

/// This process's [`Launch`], decided once by [`run`] before anything else.
static LAUNCH: OnceLock<Launch> = OnceLock::new();

/// This process's [`Launch`]. A process that never ran [`run`] — a test — is
/// what it is, a build outside the bundle, and changes nothing.
pub fn launch() -> Launch {
    LAUNCH.get().copied().unwrap_or_default()
}

/// Whether `exe` is the installed app's executable.
///
/// On macOS (`macos`) it is when it sits in `Contents/MacOS` of a bundle that
/// carries the service's launch agent — the bundle `SMAppService` registers
/// the agent from. Anywhere else it is when it sits in a `bin` directory,
/// where an installed program lives (`host/omarchy/install.sh` puts `deckd`
/// in `~/.local/bin`). A `target/` build and a test binary are neither.
pub fn installed_copy(exe: &Path, macos: bool) -> bool {
    let Some(dir) = exe.parent() else {
        return false;
    };
    if !macos {
        return dir.file_name().is_some_and(|name| name == "bin");
    }
    dir.file_name().is_some_and(|name| name == "MacOS")
        && dir.parent().is_some_and(|contents| {
            contents.file_name().is_some_and(|name| name == "Contents")
                && contents
                    .join("Library/LaunchAgents")
                    .join(DECKD_PLIST)
                    .is_file()
        })
}

/// The `deckd` beside this app's executable: `Contents/MacOS/deckd` in the
/// installed app, the one cargo built beside it in a build. `None` when there
/// is none, which first run reads as "this copy cannot look for a deck".
pub fn deckd_path() -> Option<PathBuf> {
    deckd_beside(&std::env::current_exe().ok()?)
}

/// [`deckd_path`] for the executable `exe`.
pub fn deckd_beside(exe: &Path) -> Option<PathBuf> {
    // `deckd.exe` on Windows.
    let deckd = exe
        .parent()?
        .join(format!("deckd{}", std::env::consts::EXE_SUFFIX));
    deckd.is_file().then_some(deckd)
}

/// What the window opens on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirstRun {
    /// First run takes the window.
    Show,
    /// The main window, and nothing is remembered.
    Skip,
    /// The main window, and first run is remembered as done: the service is
    /// already registered — an app updated from a release that had no first
    /// run — so first run has nothing left to set up.
    Done,
}

/// Whether the window opens on first run, from everything that decides it:
/// [`FIRST_RUN_ENV`] as it was set (`forced`), the preference that first run
/// is done, this run, and the service's launch agent as macOS reports it.
///
/// In order: the variable forces it either way; then first run is done once
/// and for good; then only the installed app, run for real, opens on it by
/// itself (a build, a harness walk or a window on a test service gets it
/// only by the variable); then an agent that is registered or waiting for
/// the owner's approval means there is nothing to set up ([`FirstRun::Done`]);
/// then an agent whose status this Mac cannot name is no reason to offer to
/// register it (and outside macOS there is none to register); and an agent
/// that is not registered is first run's to set up.
pub fn first_run_needed(
    forced: Option<&str>,
    done: bool,
    launch: Launch,
    agent: LoginItem,
) -> FirstRun {
    match forced {
        Some("1") => return FirstRun::Show,
        Some("0") => return FirstRun::Skip,
        _ => {}
    }
    if done || !launch.installed() {
        return FirstRun::Skip;
    }
    match agent {
        LoginItem::Enabled | LoginItem::RequiresApproval => FirstRun::Done,
        LoginItem::NotRegistered | LoginItem::NotFound => FirstRun::Show,
        LoginItem::Unknown(_) => FirstRun::Skip,
    }
}

/// Start the runtime, the bus and the client, then open the window.
///
/// `Err` only when the runtime thread could not be created, which on this
/// platform means the process is out of threads — there is no window to
/// report that in, so `main` prints it and exits non-zero.
pub fn run() -> std::io::Result<()> {
    // `--shots DIR`: walk the window through its scenes, save each to DIR as
    // a PNG and quit. The window is the real one; what it draws is a
    // fixture or a test service's (`shots`).
    let shots = Shots::from_args(std::env::args().skip(1));
    // What this run may change outside its window, decided before anything
    // could change it. `run` is the only setter and runs once per process.
    let _ = LAUNCH.set(Launch::detect(shots.is_some()));
    let bridge = TokioBridge::start()?;
    let (sender, receiver) = deck_gui_core::bus();
    // The app-wide bus, so a `state.get` reply can be posted from wherever
    // the view awaited it without threading a sender through every closure.
    events::install(sender.clone());
    // A harness walk dials a service only for its live scenes, and then the
    // test service on `DECKD_DATA_DIR`: the boards are drawn from fixtures,
    // and the installed service is never drawn at all.
    let state = if shots.as_ref().is_some_and(|shots| !shots.dials()) {
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
    // The window's own keys go in after gpui-component's: Tab inside a
    // dialog stays in the dialog.
    crate::shell::bind_keys(cx);
    let launch = launch();
    // A registration launchd lost — the app was moved or replaced since it
    // was made — is made again from this copy. A service that was never
    // registered is left alone: registering it is first run's **Add These**.
    #[cfg(target_os = "macos")]
    if launch.installed() {
        if let Err(error) = crate::login_item::repair_agent_if_registered() {
            eprintln!("openwalkietalkie-app: background service repair: {error}");
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
    // The app's own words, scanned after the four built-in tables, before
    // anything is drawn.
    crate::i18n::install();
    let mut prefs = Prefs::open(launch);
    let first_run = first_run_needed(
        std::env::var(FIRST_RUN_ENV).ok().as_deref(),
        prefs.first_run_done(),
        launch,
        crate::login_item::agent_status(),
    );
    if first_run == FirstRun::Done {
        prefs.set_first_run_done(true);
    }
    // A language picked on this Mac before any service answered is the one
    // spoken until a service takes it.
    let language = crate::shell::spoken(prefs.language(), state.mirror().settings.language);
    cx.set_global(prefs);
    cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
    // Before the menus: a menu item shows the key bound to its action.
    cx.bind_keys(key_bindings());
    apply_menu_language(language, cx);
    // The status item, before the window: it has to be there whether or not
    // a window ever opens, and a menu bar that refused it is not fatal — the
    // window works exactly as it did. A harness walk has none: it would sit
    // beside the owner's own for a window that quits when the walk is done.
    if !launch.harness {
        if let Some(menu) = crate::menu::install(language) {
            cx.set_global(menu);
        }
    }
    if let Err(e) = open(state, receiver, first_run == FirstRun::Show, cx) {
        eprintln!("openwalkietalkie-app: could not open the window: {e}");
        cx.quit();
        return;
    }
    cx.activate(true);
}

/// Keep the menu bar in the same language as the shared snapshot.
pub fn apply_menu_language(language: Language, cx: &mut App) {
    cx.set_menus(menus(language));
    // gpui 0.2.2 makes a menu AppKit's windows menu only when it is titled
    // "Window", so in 简体中文, 繁體中文 and 日本語 macOS added no window list
    // and no tiling items to it, and after a switch it kept the detached
    // English one. `set_menus` has built the menu bar by now.
    #[cfg(target_os = "macos")]
    crate::macos::adopt_windows_menu(WINDOW_MENU);
}

/// The Window menu's place in the menu bar: after the app's menu, Edit and
/// View ([`menus`]).
pub const WINDOW_MENU: usize = 3;

/// The menu bar in `language`, in macOS's own words for it: the app's menu
/// (Quit), **Edit** for the text fields (a macro key's label, the cloud
/// voice's key, the Local API's port and a token's name), **View** with
/// the five pages and **Window**.
///
/// Edit's items are a text field's own actions, each marked as the macOS
/// command it is ([`OsAction`]): they are enabled only while a field has
/// the keyboard, and their keys are the ones the field binds. View's are
/// dimmed under a dialog and through first run, where no page can open (the
/// shell takes them only then). Window's Close does what the close button
/// does.
pub fn menus(language: Language) -> Vec<Menu> {
    let pages = Screen::NAV
        .into_iter()
        .filter_map(|screen| {
            Some(MenuItem::Action {
                name: language.text(screen.label()).into(),
                action: page_action(screen)?,
                os_action: None,
            })
        })
        .collect();
    vec![
        Menu {
            name: "OpenWalkieTalkie".into(),
            items: vec![MenuItem::action(
                MenuCommand::Quit.label_localized(language),
                Quit,
            )],
        },
        Menu {
            name: language.text("Edit").into(),
            items: vec![
                MenuItem::os_action(language.text("Undo"), input::Undo, OsAction::Undo),
                MenuItem::os_action(language.text("Redo"), input::Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action(language.text("Cut"), input::Cut, OsAction::Cut),
                MenuItem::os_action(language.text("Copy"), input::Copy, OsAction::Copy),
                MenuItem::os_action(language.text("Paste"), input::Paste, OsAction::Paste),
                MenuItem::os_action(
                    language.text("Select All"),
                    input::SelectAll,
                    OsAction::SelectAll,
                ),
            ],
        },
        Menu {
            name: language.text("View").into(),
            items: pages,
        },
        Menu {
            name: language.text("Window").into(),
            items: vec![
                MenuItem::action(language.text("Minimize"), Minimize),
                MenuItem::action(language.text("Zoom"), Zoom),
                MenuItem::separator(),
                MenuItem::action(language.text("Close"), CloseWindow),
            ],
        },
    ]
}

/// What opens `screen` from the View menu: one of the sidebar's five pages.
/// A sub-page and the harness's own screens have none.
pub fn page_action(screen: Screen) -> Option<Box<dyn Action>> {
    match screen {
        Screen::Deck => Some(Box::new(ShowDeck)),
        Screen::FacesKeys => Some(Box::new(ShowFacesKeys)),
        Screen::Integrations => Some(Box::new(ShowIntegrations)),
        Screen::Voice => Some(Box::new(ShowVoice)),
        Screen::Service => Some(Box::new(ShowService)),
        Screen::Footprint | Screen::LocalApi | Screen::Kit | Screen::MenuPreview => None,
    }
}

/// The app's own keys: ⌘Q; ⌘1 to ⌘5 for the five pages, in the sidebar's
/// order; ⌘M and ⌘W for the window. They hold wherever the keyboard is in
/// the window, a text field included, as a menu's keys do. Edit's keys are
/// the text field's own (gpui-component binds them in the field).
pub fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-1", ShowDeck, None),
        KeyBinding::new("cmd-2", ShowFacesKeys, None),
        KeyBinding::new("cmd-3", ShowIntegrations, None),
        KeyBinding::new("cmd-4", ShowVoice, None),
        KeyBinding::new("cmd-5", ShowService, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
    ]
}

/// What the window's close button does, and Window ▸ Close: the app hides.
/// The service keeps running and the deck keeps working, and the status
/// item (or the dock icon) opens the window again.
///
/// Elsewhere it quits the window: gpui cannot hide an app there (its
/// Windows `hide` is an empty todo), so a close that only hid would do
/// nothing at all. The service is its own process and keeps the deck
/// working either way.
pub fn close_window(cx: &mut App) {
    if cfg!(target_os = "macos") {
        cx.hide();
    } else {
        cx.quit();
    }
}

/// The window's title bar. On macOS the content runs under a transparent one
/// with no title, the traffic lights on the sidebar's first row. Elsewhere a
/// transparent title bar means the app draws its own caption buttons and
/// drag area (gpui hides the system's on Windows), which this window does
/// not, so it keeps the system's title bar, with its title.
fn titlebar() -> TitlebarOptions {
    if cfg!(target_os = "macos") {
        TitlebarOptions {
            title: None,
            appears_transparent: true,
            traffic_light_position: Some(traffic_lights()),
        }
    } else {
        TitlebarOptions {
            title: Some("OpenWalkieTalkie".into()),
            appears_transparent: false,
            traffic_light_position: None,
        }
    }
}

/// The window. `first_run` hands it to first run from its first frame, step
/// 1, in place of the sidebar and the pages.
fn open(state: HostState, receiver: EventReceiver, first_run: bool, cx: &mut App) -> Result<()> {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
            cx,
        ))),
        window_min_size: Some(size(px(MIN_WIDTH), px(MIN_HEIGHT))),
        // The content runs under a transparent title bar with no title: the
        // traffic lights sit on the sidebar's first row (and first run's top
        // bar), and the shell keeps a band across the top that a double
        // click zooms the window from.
        titlebar: Some(titlebar()),
        // Opaque: the window is the palette's `win`, never the desktop
        // through glass.
        window_background: WindowBackgroundAppearance::Opaque,
        app_id: Some(APP_ID.to_string()),
        ..Default::default()
    };
    cx.open_window(options, |window, cx| {
        style::install(style::look(window, cx), cx);
        // Closing the window hides the app; the service keeps running and
        // the deck keeps working, and the status item stays in the menu
        // bar to open the window again.
        window.on_window_should_close(cx, |_, cx| {
            close_window(cx);
            false
        });
        let shell = cx.new(|cx| Shell::new(state, receiver, initial_screen(), window, cx));
        if first_run {
            shell.update(cx, |shell, cx| shell.begin_first_run(1, window, cx));
        }
        if cx.has_global::<Shots>() {
            crate::shots::start(shell.clone(), window, cx);
        }
        // The window's first layer must be a `gpui_component::Root`: it owns
        // the notification state the screens raise.
        cx.new(|cx| Root::new(shell, window, cx))
    })?;
    // A harness walk's window must stay uncovered for every capture, even
    // when macOS refuses this process the front (`macos::float_for_capture`).
    #[cfg(target_os = "macos")]
    if cx.has_global::<Shots>() {
        crate::macos::float_for_capture();
    }
    Ok(())
}

/// Bring the window back (the dock icon, the status item's **Open**).
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
/// loaded agent and restarts a wedged one, which is the two things the
/// not-running strip can mean.
///
/// Registering the agent in the first place is `SMAppService` and first
/// run's job (§10.2); this button is for the agent that is installed and not
/// running.
#[cfg(unix)]
pub fn kickstart_argv() -> Vec<String> {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    vec![
        "kickstart".to_string(),
        "-k".to_string(),
        format!("gui/{uid}/{DECKD_LABEL}"),
    ]
}

/// Run [`kickstart_argv`] off the UI thread and answer on the returned
/// channel — which is what the [`Tokio`] global is for. The `waitpid` goes
/// to `spawn_blocking` and not to a runtime worker: the workspace's tokio
/// pin carries no `process` feature, so this is a blocking `std::process`
/// call and parking one of the two workers on it would stall the socket.
///
/// Any run but the installed app's answers why it did nothing, and runs no
/// `launchctl` at all ([`Launch::hands_off`]).
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

/// Where a [`start_service`] answer lands.
pub type Receiver = async_channel::Receiver<std::result::Result<(), String>>;

/// The blocking half on Windows, which has no launch agent yet: start the
/// `deckd` beside this app as `deckd run`, detached and windowless. A
/// second copy finds the lock held and exits, so pressing Start on a
/// running service changes nothing.
#[cfg(windows)]
fn kickstart() -> std::result::Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    if let Some(why) = launch().hands_off() {
        return Err(why.to_string());
    }
    let deckd = deckd_path().ok_or("there is no deckd beside this app")?;
    std::process::Command::new(deckd)
        .arg("run")
        .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| e.to_string())
}

/// The blocking half: spawn `launchctl` and read its verdict.
#[cfg(unix)]
fn kickstart() -> std::result::Result<(), String> {
    // The label is the owner's installed agent's whichever copy asks, so
    // this is the last place a build, a harness walk or a window on a test
    // service could restart his service from. None of them does.
    if let Some(why) = launch().hands_off() {
        return Err(why.to_string());
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A menu as its words, each item's action and the macOS command it is
    /// marked as: a separator is `—`.
    fn items(menu: &Menu) -> Vec<(String, &'static str, &'static str)> {
        menu.items
            .iter()
            .map(|item| match item {
                MenuItem::Action {
                    name,
                    action,
                    os_action,
                } => {
                    let command = match os_action {
                        Some(OsAction::Cut) => "cut",
                        Some(OsAction::Copy) => "copy",
                        Some(OsAction::Paste) => "paste",
                        Some(OsAction::SelectAll) => "select all",
                        Some(OsAction::Undo) => "undo",
                        Some(OsAction::Redo) => "redo",
                        None => "",
                    };
                    (name.to_string(), action.name(), command)
                }
                MenuItem::Separator => ("—".to_string(), "", ""),
                MenuItem::Submenu(_) | MenuItem::SystemMenu(_) => ("?".to_string(), "", ""),
            })
            .collect()
    }

    /// The menu bar's titles.
    fn titles(menus: &[Menu]) -> Vec<String> {
        menus.iter().map(|menu| menu.name.to_string()).collect()
    }

    #[test]
    fn the_menu_bar_is_the_apps_menu_edit_view_and_window() {
        crate::i18n::install();
        let menus = menus(Language::English);
        assert_eq!(
            titles(&menus),
            ["OpenWalkieTalkie", "Edit", "View", "Window"]
        );
        let row = |name: &str, action: &'static str, command: &'static str| {
            (name.to_string(), action, command)
        };
        assert_eq!(
            items(&menus[0]),
            [row("Quit OpenWalkieTalkie", "openwalkietalkie::Quit", "")]
        );
        // A text field's own actions, each marked as the Mac's command.
        assert_eq!(
            items(&menus[1]),
            [
                row("Undo", "input::Undo", "undo"),
                row("Redo", "input::Redo", "redo"),
                row("—", "", ""),
                row("Cut", "input::Cut", "cut"),
                row("Copy", "input::Copy", "copy"),
                row("Paste", "input::Paste", "paste"),
                row("Select All", "input::SelectAll", "select all"),
            ]
        );
        // The sidebar's five pages, in its order.
        assert_eq!(
            items(&menus[2]),
            [
                row("Deck", "openwalkietalkie::ShowDeck", ""),
                row("Faces & Keys", "openwalkietalkie::ShowFacesKeys", ""),
                row("Integrations", "openwalkietalkie::ShowIntegrations", ""),
                row("Voice", "openwalkietalkie::ShowVoice", ""),
                row("Service", "openwalkietalkie::ShowService", ""),
            ]
        );
        assert_eq!(
            items(&menus[3]),
            [
                row("Minimize", "openwalkietalkie::Minimize", ""),
                row("Zoom", "openwalkietalkie::Zoom", ""),
                row("—", "", ""),
                row("Close", "openwalkietalkie::CloseWindow", ""),
            ]
        );
        for (i, screen) in Screen::NAV.into_iter().enumerate() {
            assert_eq!(
                page_action(screen).map(|action| action.name()),
                Some(items(&menus[2])[i].1),
                "{screen:?}"
            );
        }
        // A sub-page and the harness's own screens are not in the menu.
        for screen in [
            Screen::Footprint,
            Screen::LocalApi,
            Screen::Kit,
            Screen::MenuPreview,
        ] {
            assert!(page_action(screen).is_none(), "{screen:?}");
        }
    }

    /// AppKit is told which menu is the Window menu by its place in the menu
    /// bar (`macos::adopt_windows_menu`), because gpui tells it only for a
    /// menu titled "Window" in English. The place holds in every language.
    #[test]
    fn the_window_menu_is_at_its_place_in_every_language() {
        crate::i18n::install();
        for (language, window) in [
            (Language::English, "Window"),
            (Language::SimplifiedChinese, "窗口"),
            (Language::TraditionalChinese, "視窗"),
            (Language::Japanese, "ウインドウ"),
        ] {
            assert_eq!(
                menus(language)[WINDOW_MENU].name.as_ref(),
                window,
                "{language:?}"
            );
        }
    }

    #[test]
    fn the_menus_say_what_the_macs_own_menus_say_in_each_language() {
        crate::i18n::install();
        for (language, edit, view, window) in [
            (Language::SimplifiedChinese, "编辑", "显示", "窗口"),
            (Language::TraditionalChinese, "編輯", "顯示方式", "視窗"),
            (Language::Japanese, "編集", "表示", "ウインドウ"),
        ] {
            let menus = menus(language);
            assert_eq!(
                titles(&menus),
                ["OpenWalkieTalkie", edit, view, window],
                "{language:?}"
            );
            // Every item is in the language, in the same order and with the
            // same action as in English: nothing is left in English.
            let english = menus_items(Language::English);
            let said = menus_items(language);
            assert_eq!(said.len(), english.len());
            for (said, english) in said.iter().zip(&english) {
                assert_eq!(said.1, english.1, "{language:?}");
                if english.0 != "—" {
                    assert_ne!(said.0, english.0, "{language:?}");
                }
            }
        }
        let japanese = menus(Language::Japanese);
        assert_eq!(items(&japanese[3])[0].0, "しまう");
        assert_eq!(items(&japanese[3])[1].0, "拡大/縮小");
        let traditional = menus(Language::TraditionalChinese);
        assert_eq!(items(&traditional[3])[0].0, "縮到最小");
        assert_eq!(items(&traditional[1])[4].0, "拷貝");
        let simplified = menus(Language::SimplifiedChinese);
        assert_eq!(items(&simplified[1])[4].0, "拷贝");
        assert_eq!(items(&simplified[3])[3].0, "关闭");
    }

    /// Every item of every menu bar but the app's own, in `language`.
    fn menus_items(language: Language) -> Vec<(String, &'static str, &'static str)> {
        menus(language)[1..].iter().flat_map(items).collect()
    }

    #[test]
    fn the_pages_and_the_window_have_the_macs_keys_wherever_the_keyboard_is() {
        let keys: Vec<(String, &str)> = key_bindings()
            .iter()
            .map(|binding| {
                let strokes: Vec<String> = binding
                    .keystrokes()
                    .iter()
                    .map(|stroke| stroke.unparse())
                    .collect();
                (strokes.join(" "), binding.action().name())
            })
            .collect();
        let key = |stroke: &str, action: &'static str| (stroke.to_string(), action);
        assert_eq!(
            keys,
            [
                key("cmd-q", "openwalkietalkie::Quit"),
                key("cmd-1", "openwalkietalkie::ShowDeck"),
                key("cmd-2", "openwalkietalkie::ShowFacesKeys"),
                key("cmd-3", "openwalkietalkie::ShowIntegrations"),
                key("cmd-4", "openwalkietalkie::ShowVoice"),
                key("cmd-5", "openwalkietalkie::ShowService"),
                key("cmd-m", "openwalkietalkie::Minimize"),
                key("cmd-w", "openwalkietalkie::CloseWindow"),
            ]
        );
        // None is bound in a context: a menu's key holds wherever the
        // keyboard is, a text field included.
        assert!(key_bindings()
            .iter()
            .all(|binding| binding.predicate().is_none()));
        // Every command of the app's own menus has a key but Zoom, which
        // has none on any Mac; Edit's keys are the text field's.
        let bound: Vec<&str> = key_bindings()
            .iter()
            .map(|binding| binding.action().name())
            .collect();
        for menu in [0, 2, 3] {
            for (name, action, _) in items(&menus(Language::English)[menu]) {
                if !action.is_empty() && name != "Zoom" {
                    assert!(bound.contains(&action), "{name}");
                }
            }
        }
    }

    #[test]
    fn every_word_the_menus_say_has_a_row() {
        let source = include_str!("app.rs");
        assert_eq!(crate::i18n::missing_rows(source), Vec::<String>::new());
        // Pinned, so a lookup the scanner stopped seeing fails here rather
        // than passing as menus with less to translate: Edit's title and its
        // six items, View's title, and Window's with its three.
        assert_eq!(crate::i18n::literals(source).len(), 12);
    }

    #[test]
    fn the_window_is_the_designs_size_and_can_never_be_squeezed_past_its_minimum() {
        use crate::ui::{PAGE_SIDE, PAGE_WIDTH};
        assert_eq!((WINDOW_WIDTH, WINDOW_HEIGHT), (1040., 780.));
        assert_eq!((MIN_WIDTH, MIN_HEIGHT), (1040., 600.));
        const { assert!(MIN_WIDTH <= WINDOW_WIDTH && MIN_HEIGHT < WINDOW_HEIGHT) };
        // The narrowest window still holds the sidebar and a whole column:
        // a page is never laid out narrower than the design drew it.
        assert_eq!(
            crate::shell::SIDEBAR_WIDTH + PAGE_WIDTH + 2. * PAGE_SIDE,
            MIN_WIDTH
        );
        // The harness draws the boards at the window's own size.
        assert_eq!(crate::shots::BOARD_SIZE, (WINDOW_WIDTH, WINDOW_HEIGHT));
        // The close button's centre on the design's (24, 26), for buttons
        // 14 px across.
        assert_eq!((TRAFFIC_LIGHTS.0 + 7., TRAFFIC_LIGHTS.1 + 7.), (24., 26.));
        assert_eq!(APP_ID, "ai.cohdl.openwalkietalkie");
    }

    #[cfg(unix)]
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

    /// The installed app, and the three runs that are not it.
    fn runs() -> (Launch, [Launch; 3]) {
        let installed = Launch {
            bundled: true,
            ..Launch::default()
        };
        let others = [
            // A build outside the bundle: `cargo run`, a test.
            Launch::default(),
            Launch {
                harness: true,
                ..installed
            },
            Launch {
                data_dir: true,
                ..installed
            },
        ];
        (installed, others)
    }

    /// An empty directory of the test's own under the system's temporary
    /// one, removed again when the test is done with it.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!(
                "openwalkietalkie-app-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn only_the_installed_app_run_for_real_reaches_past_its_window() {
        let (installed, others) = runs();
        assert!(installed.installed());
        assert_eq!(installed.hands_off(), None);
        for launch in others {
            assert!(!launch.installed(), "{launch:?}");
            assert!(launch.hands_off().is_some_and(|why| !why.is_empty()));
        }
        // Each fact alone is enough, whatever the others say.
        for bundled in [false, true] {
            for harness in [false, true] {
                for data_dir in [false, true] {
                    let launch = Launch {
                        bundled,
                        harness,
                        data_dir,
                    };
                    assert_eq!(launch.installed(), bundled && !harness && !data_dir);
                }
            }
        }
    }

    #[test]
    fn a_process_that_never_booted_is_a_build_and_changes_nothing() {
        // Nothing in a test calls `run`, so the tests themselves are held to
        // the rule they check: no test is ever the installed app.
        assert_eq!(launch(), Launch::default());
        assert!(!launch().installed());
        let exe = std::env::current_exe().unwrap();
        assert!(!installed_copy(&exe, cfg!(target_os = "macos")), "{exe:?}");
    }

    #[test]
    fn the_installed_copy_is_the_bundle_that_carries_the_agent_or_a_bin_directory() {
        let scratch = Scratch::new("bundle");
        let contents = scratch.0.join("OpenWalkieTalkie.app/Contents");
        let exe = contents.join("MacOS/OpenWalkieTalkie");
        assert!(
            !installed_copy(&exe, true),
            "a bundle without the agent's plist is not one SMAppService can register from"
        );
        let agents = contents.join("Library/LaunchAgents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join(DECKD_PLIST), "").unwrap();
        assert!(installed_copy(&exe, true));
        // The plist is there, but the executable is not the bundle's own.
        assert!(!installed_copy(
            &contents.join("Resources/OpenWalkieTalkie"),
            true
        ));
        assert!(!installed_copy(
            &scratch.0.join("Contents/MacOS/OpenWalkieTalkie"),
            true
        ));
        assert!(!installed_copy(
            Path::new("/Users/owner/openmicrodial/target/host/aarch64-apple-darwin/debug/openwalkietalkie-app"),
            true
        ));
        assert!(!installed_copy(Path::new("openwalkietalkie-app"), true));
        // Outside macOS: a `bin` directory, and nothing else.
        assert!(installed_copy(
            Path::new("/home/owner/.local/bin/openwalkietalkie-app"),
            false
        ));
        assert!(installed_copy(
            Path::new("/usr/bin/openwalkietalkie-app"),
            false
        ));
        assert!(!installed_copy(
            Path::new("/home/owner/openmicrodial/target/release/openwalkietalkie-app"),
            false
        ));
        assert!(!installed_copy(&exe, false), "a bundle means nothing there");
        assert_eq!(DECKD_PLIST, format!("{DECKD_LABEL}.plist"));
    }

    #[test]
    fn deckd_is_the_file_beside_the_executable_or_none() {
        let scratch = Scratch::new("deckd");
        let exe = scratch.0.join("OpenWalkieTalkie");
        let deckd = scratch.0.join("deckd");
        assert_eq!(deckd_beside(&exe), None);
        std::fs::create_dir(&deckd).unwrap();
        assert_eq!(deckd_beside(&exe), None, "a directory is not a deckd");
        std::fs::remove_dir(&deckd).unwrap();
        std::fs::write(&deckd, "").unwrap();
        assert_eq!(deckd_beside(&exe), Some(deckd));
        assert_eq!(deckd_beside(Path::new("/")), None);
    }

    #[test]
    fn only_the_installed_app_has_the_service_look_for_firmware() {
        let (installed, others) = runs();
        for force in [false, true] {
            assert_eq!(
                installed.outgoing(UiAction::FirmwareCheck { force }),
                UiAction::FirmwareCheck { force }
            );
            // A check asked for, forced or not: no other run has the service
            // open the deck or fetch the manifest.
            for launch in others {
                assert_eq!(
                    launch.outgoing(UiAction::FirmwareCheck { force }),
                    UiAction::FirmwareStatus,
                    "{launch:?}"
                );
            }
        }
        // Everything else goes as it was asked for, in every run.
        let install = UiAction::FirmwareInstall {
            board: "Rev B".into(),
            serial: "0123456789AB".into(),
            version: "0.13.0".into(),
            allow_bootloader_update: false,
        };
        for launch in [installed, others[0], others[1], others[2]] {
            for action in [
                UiAction::FirmwareStatus,
                install.clone(),
                UiAction::StateGet,
                UiAction::ServiceInfo,
            ] {
                assert_eq!(launch.outgoing(action.clone()), action);
            }
        }
        // And what replaces the check is a read the service already answers
        // with the same reply.
        assert_eq!(UiAction::FirmwareStatus.request().0, "firmware.status");
    }

    #[test]
    fn the_first_run_variable_forces_it_either_way() {
        let (installed, others) = runs();
        for launch in [installed, others[0], others[1], others[2]] {
            for done in [false, true] {
                for agent in [LoginItem::NotRegistered, LoginItem::Enabled] {
                    assert_eq!(
                        first_run_needed(Some("1"), done, launch, agent),
                        FirstRun::Show
                    );
                    assert_eq!(
                        first_run_needed(Some("0"), done, launch, agent),
                        FirstRun::Skip
                    );
                }
            }
        }
        // Any other value is no value.
        for value in ["", "yes", "true", " 1"] {
            assert_eq!(
                first_run_needed(Some(value), false, installed, LoginItem::NotRegistered),
                FirstRun::Show,
                "{value:?}"
            );
            assert_eq!(
                first_run_needed(Some(value), false, others[0], LoginItem::NotRegistered),
                FirstRun::Skip,
                "{value:?}"
            );
        }
    }

    #[test]
    fn first_run_opens_by_itself_only_on_an_installed_app_with_nothing_set_up() {
        let (installed, others) = runs();
        for agent in [LoginItem::NotRegistered, LoginItem::NotFound] {
            assert_eq!(
                first_run_needed(None, false, installed, agent),
                FirstRun::Show
            );
            // Done is done.
            assert_eq!(
                first_run_needed(None, true, installed, agent),
                FirstRun::Skip
            );
            // A build, a harness walk and a window on a test service open
            // on the pages.
            for launch in others {
                assert_eq!(
                    first_run_needed(None, false, launch, agent),
                    FirstRun::Skip,
                    "{launch:?}"
                );
            }
        }
        // A status this Mac has no name for, or a platform with no agent to
        // register: no reason to offer to register one.
        assert_eq!(
            first_run_needed(None, false, installed, LoginItem::Unknown(-1)),
            FirstRun::Skip
        );
    }

    #[test]
    fn an_app_whose_service_is_already_registered_has_finished_first_run() {
        let (installed, others) = runs();
        // Updated from a release with no first run: the service is there, or
        // waiting for the owner's approval. Nothing to set up, and it is
        // remembered so the next start does not ask macOS again.
        for agent in [LoginItem::Enabled, LoginItem::RequiresApproval] {
            assert_eq!(
                first_run_needed(None, false, installed, agent),
                FirstRun::Done
            );
            assert_eq!(
                first_run_needed(None, true, installed, agent),
                FirstRun::Skip
            );
            for launch in others {
                assert_eq!(
                    first_run_needed(None, false, launch, agent),
                    FirstRun::Skip,
                    "a run that is not the installed app remembers nothing"
                );
            }
        }
    }
}
