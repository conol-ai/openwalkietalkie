//! The window itself: the mirror, the bus, the frame around the pages, and
//! the dialogs that can arrive whichever page is up.
//!
//! One view owns all of it. [`Shell`] holds the single [`HostState`], pumps
//! the single bus and is the only thing that calls the service, and each
//! page is a pane it renders from its own small piece of view state. That is
//! deliberate: views each with a client would be mirrors that could
//! disagree, and the reducer's whole point is that there is one.
//!
//! **Every page keeps one contract with it.** A page module
//! ([`crate::deck`], [`crate::faces`], [`crate::integrations`],
//! [`crate::voice`], [`crate::service`], [`crate::firstrun`]) provides its
//! pane and, on `Shell`, `refresh_<p>`, `render_<p>`, `<p>_dialog`,
//! `<p>_dialog_closed`, `localize_<p>`, `on_<p>_event`, `tick_<p>` and
//! `stage_<p>`; the frame ([`crate::frame`], [`crate::menu`]) provides the
//! sidebar, the strips and the status item. The shell reaches the pages
//! through those functions and nothing else, so a page can be rebuilt
//! without this file changing.
//!
//! The view holds no state about the deck. Everything it draws comes out of
//! the mirror, and every control sends a [`UiAction`] — `HostState::send`
//! takes `&self` and cannot write to the mirror, so a click's only effect on
//! this window is the events the service sends back.
//!
//! **The dialogs are the window's own**, drawn by [`Shell::render`] from that
//! state rather than handed to gpui-component's sheet and dialog layers. A
//! layer builder runs while this view is leased and cannot read it, so it
//! had to be given a copy of everything it showed; a dialog drawn here reads
//! the live state instead. One is up at a time: the shell's own question
//! before something that cannot be taken back ([`Shell::ask_first`]), or a
//! dialog a page draws, named by its id ([`Shell::open_dialog`]).
//!
//! Every `render_*` takes `&Context<Self>` and not `&mut` on purpose: a
//! renderer that cannot borrow the context mutably cannot reach the mirror,
//! and the compiler keeps that promise instead of a comment.
//!
//! **The keyboard has a home here.** The window's root holds a focus handle
//! of its own, which is where the keyboard is when no control has it: Tab
//! only moves from something inside the window (`gpui_component::Root`
//! binds it there), so a control that went away while it had the keyboard,
//! or a menu that closed, would otherwise leave Tab doing nothing. Each
//! frame puts a keyboard that is nowhere back home. While a dialog is up,
//! Tab and Shift-Tab go round the dialog's own controls and never reach
//! the page under the scrim.
use crate::app;
use crate::deck::DeckPane;
use crate::faces::FacesPane;
use crate::firstrun::FirstRunPane;
use crate::integrations::IntegrationsPane;
use crate::kitboard::KitPane;
use crate::prefs::Prefs;
use crate::service::ServicePane;
use crate::style::{self, palette};
use crate::ui::{ic, para, Btn, Type as _};
use crate::voice::VoicePane;
use deck_client::ClientError;
use deck_gui_core::i18n::Language;
use deck_gui_core::{
    events, ApiEvent, AppEvent, EventReceiver, HostEffect, HostState, SheetKind, UiAction,
};
use deck_model::{Link, SetKey};
use gpui::{
    div, ease_out_quint, prelude::*, px, relative, Animation, AnimationExt as _, AnyElement, App,
    Context, Div, ElementId, FocusHandle, KeyBinding, KeyDownEvent, ScrollHandle, Stateful,
    Subscription, Task, Window,
};
use gpui_component::notification::Notification;
use gpui_component::{h_flex, v_flex, Root, WindowExt as _};
use serde_json::Value;
use std::time::Duration;

/// How often the window repaints itself with nothing new from the service,
/// and how often each page is given its tick: ages and countdowns are read
/// to the second.
pub const TICK: Duration = Duration::from_secs(1);

/// How long the window waits before asking for the snapshot again when a
/// `state.get` did not come back with one (an error, or `-32013 degraded`).
///
/// The reducer asks **once** per hole and then goes quiet — with a
/// `state.get` outstanding it issues no second `StateGet`, and while the
/// mirror is frozen every later event is another hole, so nothing on the
/// service's side can ever re-arm it. Re-asking is therefore the window's
/// job, and without it one refused `state.get` freezes the window's mirror
/// for as long as it is open.
const RESYNC_RETRY: Duration = Duration::from_secs(2);

/// The sidebar, its 1 px rule included: the rule is a border, and a border
/// is laid out inside the width it borders.
pub const SIDEBAR_WIDTH: f32 = 212.;

/// The invisible band across the top of the window that a double click
/// zooms the window from, as a title bar's would. Nothing is laid out
/// around it: it lies behind the sidebar's first row, where the traffic
/// lights are, and the main column's top padding, both 28 px or more.
pub const TITLEBAR_HEIGHT: f32 = 28.;

/// How wide the shell's own question is drawn.
const SHEET_WIDTH: f32 = 420.;

/// The window's screens. The first five are the sidebar's rows; Footprint
/// and Local API are Service's own sub-pages and keep its row outlined; the
/// Tokens scene and the menu preview are reached only by the harness or by
/// [`SCREEN_ENV`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Deck,
    FacesKeys,
    Integrations,
    Voice,
    Service,
    Footprint,
    LocalApi,
    /// The kit's parts in every state (the Tokens board).
    Kit,
    /// The status item's menu, drawn in the window (the MenuBar board).
    MenuPreview,
}

impl Screen {
    pub const ALL: [Screen; 9] = [
        Screen::Deck,
        Screen::FacesKeys,
        Screen::Integrations,
        Screen::Voice,
        Screen::Service,
        Screen::Footprint,
        Screen::LocalApi,
        Screen::Kit,
        Screen::MenuPreview,
    ];

    /// The sidebar's rows, in order.
    pub const NAV: [Screen; 5] = [
        Screen::Deck,
        Screen::FacesKeys,
        Screen::Integrations,
        Screen::Voice,
        Screen::Service,
    ];

    /// Its English name, which is also its key in the tables.
    pub const fn label(self) -> &'static str {
        match self {
            Screen::Deck => "Deck",
            Screen::FacesKeys => "Faces & Keys",
            Screen::Integrations => "Integrations",
            Screen::Voice => "Voice",
            Screen::Service => "Service",
            Screen::Footprint => "Footprint",
            Screen::LocalApi => "Local API",
            Screen::Kit => "Tokens",
            Screen::MenuPreview => "Menu bar",
        }
    }

    /// The name [`SCREEN_ENV`] calls it by.
    pub const fn slug(self) -> &'static str {
        match self {
            Screen::Deck => "deck",
            Screen::FacesKeys => "faces",
            Screen::Integrations => "integrations",
            Screen::Voice => "voice",
            Screen::Service => "service",
            Screen::Footprint => "footprint",
            Screen::LocalApi => "local-api",
            Screen::Kit => "kit",
            Screen::MenuPreview => "menu",
        }
    }

    /// The `id` its sidebar row is drawn with.
    pub const fn id(self) -> &'static str {
        match self {
            Screen::Deck => "nav-deck",
            Screen::FacesKeys => "nav-faces",
            Screen::Integrations => "nav-integrations",
            Screen::Voice => "nav-voice",
            Screen::Service => "nav-service",
            Screen::Footprint => "nav-footprint",
            Screen::LocalApi => "nav-local-api",
            Screen::Kit => "nav-kit",
            Screen::MenuPreview => "nav-menu",
        }
    }

    /// The glyph its sidebar row leads with.
    pub const fn icon(self) -> &'static str {
        match self {
            Screen::Deck => ic::DECK,
            Screen::FacesKeys => ic::WIDGET,
            Screen::Integrations => ic::PLUG,
            Screen::Voice => ic::MICROPHONE,
            Screen::Service | Screen::Footprint | Screen::LocalApi => ic::SERVER,
            Screen::Kit => ic::TUNING,
            Screen::MenuPreview => ic::LIST,
        }
    }

    /// The sidebar row drawn as the current one while this screen is up: a
    /// sub-page keeps its page's row, and the harness's own screens have
    /// none.
    pub const fn nav(self) -> Option<Screen> {
        match self {
            Screen::Deck
            | Screen::FacesKeys
            | Screen::Integrations
            | Screen::Voice
            | Screen::Service => Some(self),
            Screen::Footprint | Screen::LocalApi => Some(Screen::Service),
            Screen::Kit | Screen::MenuPreview => None,
        }
    }

    /// A screen by its [`slug`](Screen::slug) or its label, in either case.
    /// Unknown names are the Deck, which is what the window opens on anyway.
    pub fn parse(name: &str) -> Screen {
        let name = name.trim();
        Screen::ALL
            .into_iter()
            .find(|s| s.slug().eq_ignore_ascii_case(name) || s.label().eq_ignore_ascii_case(name))
            .unwrap_or(Screen::Deck)
    }
}

/// Which screen the window opens on. It is the Deck unless [`SCREEN_ENV`]
/// says otherwise, which is how a run gate proves that a screen renders:
/// `runtime_shaders` defers shader compilation to window-open time, so only
/// a screen that has actually been drawn has been shown to work.
pub fn initial_screen() -> Screen {
    std::env::var(SCREEN_ENV)
        .ok()
        .map_or(Screen::Deck, |name| Screen::parse(&name))
}

/// See [`initial_screen`].
pub const SCREEN_ENV: &str = "OPENWALKIETALKIE_SCREEN";

/// What [`Shell::ask_first`] asks: a title, a sentence, and the word on the
/// button that goes ahead. **Cancel** is always there and is the default.
pub(crate) struct Sheet {
    pub title: String,
    pub body: String,
    pub confirm: String,
    /// The going-ahead button is drawn as a destructive one.
    pub danger: bool,
}

/// What a sheet's going-ahead button runs.
type Then = Box<dyn FnOnce(&mut Shell, &mut Window, &mut Context<Shell>)>;

/// The dialog in front of the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dialog {
    /// The shell's own question ([`Shell::ask_first`]).
    Sheet,
    /// A dialog a page draws, by the id the page opened it under
    /// ([`Shell::open_dialog`]).
    Page(&'static str),
}

impl Dialog {
    /// Whether Escape and a click on the scrim close it. A token being shown
    /// does not close that way: the service keeps only a hash of it, so the
    /// dialog is the one copy there will ever be, and only its own **Done**
    /// may end it.
    pub(crate) fn dismissable(self) -> bool {
        !matches!(self, Dialog::Page("token"))
    }
}

gpui::actions!(
    owt_dialog,
    [
        /// Tab while a dialog is up: its next control.
        NextControl,
        /// Shift-Tab while a dialog is up: its previous control.
        PreviousControl,
    ]
);

/// The dialog layer's key context: Tab and Shift-Tab are bound in it, over
/// `gpui_component::Root`'s own Tab (a deeper context wins).
const DIALOG_KEYS: &str = "OwtDialog";

/// The window's own keys, bound once at boot after `gpui_component`'s.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", NextControl, Some(DIALOG_KEYS)),
        KeyBinding::new("shift-tab", PreviousControl, Some(DIALOG_KEYS)),
    ]);
}

/// Where Tab (`forward`) or Shift-Tab goes among `count` controls from the
/// one at `at`, if one of them has the keyboard: round and round. From
/// none, the first or the last. `None` when there are none.
pub(crate) fn cycle(at: Option<usize>, count: usize, forward: bool) -> Option<usize> {
    if count == 0 {
        return None;
    }
    Some(match (at.filter(|&i| i < count), forward) {
        (Some(i), true) => (i + 1) % count,
        (Some(i), false) => (i + count - 1) % count,
        (None, true) => 0,
        (None, false) => count - 1,
    })
}

pub struct Shell {
    pub(crate) state: HostState,
    pub(crate) screen: Screen,
    // The pages' panes. A pane is read by its own page and by whatever the
    // contract lets other pages ask of it.
    pub(crate) deck: DeckPane,
    pub(crate) faces: FacesPane,
    pub(crate) integrations: IntegrationsPane,
    pub(crate) voice: VoicePane,
    pub(crate) service: ServicePane,
    pub(crate) first_run: FirstRunPane,
    /// The Tokens scene's own state ([`crate::kitboard`]).
    pub(crate) kit: KitPane,
    /// Where the keyboard is while no control has it: the window's root.
    root_focus: FocusHandle,
    /// The dialog that is up, if any.
    dialog: Option<Dialog>,
    /// The question [`Dialog::Sheet`] shows, and what going ahead runs.
    sheet: Option<(Sheet, Then)>,
    /// Where Escape goes while a dialog is up.
    dialog_focus: FocusHandle,
    /// Bumped whenever a dialog opens, so its entrance plays again.
    dialog_seq: u64,
    /// The pages' scroll. One handle, because one page is up at a time, and
    /// a page opened from the sidebar starts at its top.
    pub(crate) page_scroll: ScrollHandle,
    /// The width right of the sidebar, as of this frame. Pages size their
    /// columns from it in pixels (see `ui::page`).
    pub(crate) content_width: f32,
    /// The connection as the last event left it, so a socket that came back
    /// can re-ask for what the open page shows: nothing folds a call's reply
    /// into the mirror, so a reconnect leaves a page quietly stale until
    /// something asks.
    was_connected: bool,
    /// A language picked on this Mac that the service has not taken yet
    /// (first run's popup, before any service answered). The window speaks
    /// it until the service has it; see [`Shell::language`]. It is the
    /// `language` preference, held here so the window can read it without a
    /// context, and kept there so a pick outlives a quit before any service
    /// has answered.
    language_choice: Option<Language>,
    /// The bus pump, the repaint tick, the pages' own subscriptions and the
    /// appearance watch. Held because a dropped `Task`/`Subscription` is a
    /// cancelled one.
    _pump: Task<()>,
    _tick: Task<()>,
    _keys: Vec<Subscription>,
}

impl Shell {
    /// The language the window speaks: a choice made on this Mac while the
    /// service has not taken it yet, otherwise the deck's own setting.
    pub(crate) fn language(&self) -> Language {
        spoken(self.language_choice, self.state.mirror().settings.language)
    }

    pub(crate) fn tr<'a>(&self, english: &'a str) -> &'a str {
        self.language().text(english)
    }

    pub(crate) fn fmt(&self, english: &str, values: &[&str]) -> String {
        self.language().format(english, values)
    }

    /// Whether a deck is linked: the service is there and its link is up. A
    /// degraded link counts — the deck is linked and silent, which the pages
    /// say as such.
    pub(crate) fn linked(&self) -> bool {
        self.state.connected() && self.state.mirror().device.link != Link::Offline
    }

    /// This app's own preferences ([`crate::prefs`]): the case colour, first
    /// run done, the language picked before any service answered.
    pub(crate) fn prefs<'a>(&self, cx: &'a App) -> &'a Prefs {
        crate::prefs::prefs(cx)
    }

    /// Change this app's own preferences, keep them at once, and redraw. The
    /// language is not changed here but through [`Shell::choose_language`],
    /// which also decides whether the service is told.
    pub(crate) fn update_prefs(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Prefs)) {
        cx.update_global::<Prefs, _>(|prefs, _| change(prefs));
        cx.notify();
    }

    pub fn new(
        state: HostState,
        bus: EventReceiver,
        screen: Screen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (deck, deck_keys) = DeckPane::new(window, cx);
        let (faces, faces_keys) = FacesPane::new(window, cx);
        let (integrations, integrations_keys) = IntegrationsPane::new(window, cx);
        let (voice, voice_keys) = VoicePane::new(window, cx);
        let (service, service_keys) = ServicePane::new(window, cx);
        let (first_run, first_run_keys) = FirstRunPane::new(window, cx);
        let kit = KitPane::new(window, cx);
        // The bus, awaited on gpui's own foreground executor: an
        // `async_channel` receiver is a plain future and needs no runtime,
        // which is why this is not a poll on the repaint tick.
        let pump = cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = bus.recv().await {
                if this
                    .update_in(cx, |this, window, cx| this.on_app_event(event, window, cx))
                    .is_err()
                {
                    // The window is gone; so is the reason to read the bus.
                    break;
                }
            }
        });
        let tick = cx.spawn_in(window, async move |this, cx| {
            let mut beat = 0u64;
            loop {
                cx.background_executor().timer(TICK).await;
                beat = beat.wrapping_add(1);
                let ticked = this.update_in(cx, |this, window, cx| {
                    this.tick_pages(beat, window, cx);
                    cx.notify();
                });
                if ticked.is_err() {
                    break;
                }
            }
        });
        // Light and dark follow the Mac (or `OPENWALKIETALKIE_APPEARANCE`,
        // which pins the Mac's answer for this app): the palette is
        // re-derived and re-installed on every change, never patched.
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            style::install(style::Palette::for_appearance(window.appearance()), cx);
            window.refresh();
        });
        // The screen the window opens on asks for what it draws — from
        // *after* construction, because `go` needs the `Shell` that is still
        // being built here. The keyboard starts at home, so the first Tab
        // reaches the first control.
        cx.spawn_in(window, async move |this, cx| {
            this.update_in(cx, move |this, window, cx| {
                window.focus(&this.root_focus);
                this.go(screen, window, cx);
                this.localize_pages(window, cx);
            })
            .ok();
        })
        .detach();
        let mut keys = deck_keys;
        for more in [
            faces_keys,
            integrations_keys,
            voice_keys,
            service_keys,
            first_run_keys,
        ] {
            keys.extend(more);
        }
        keys.push(appearance);
        let language_choice = crate::prefs::prefs(cx).language();
        Shell {
            state,
            screen,
            deck,
            faces,
            integrations,
            voice,
            service,
            first_run,
            kit,
            root_focus: cx.focus_handle(),
            dialog: None,
            sheet: None,
            dialog_focus: cx.focus_handle(),
            dialog_seq: 0,
            page_scroll: ScrollHandle::new(),
            content_width: crate::app::WINDOW_WIDTH - SIDEBAR_WIDTH,
            was_connected: false,
            language_choice,
            _pump: pump,
            _tick: tick,
            _keys: keys,
        }
    }

    /// One event from the bus: fold it, do what the reducer asked, then let
    /// every page and the status item see the result.
    fn on_app_event(&mut self, event: AppEvent, window: &mut Window, cx: &mut Context<Self>) {
        // A click on the status item is an action and the reducer issues
        // none, so it is taken here. It still comes through the bus, because
        // `muda` runs its handler on the menu's own thread and the bus is the
        // one path into this view from off it.
        if let AppEvent::Menu(command) = event {
            self.on_menu(command, window, cx);
            return;
        }
        let previous_language = self.language();
        for effect in self.state.handle_event(event) {
            self.apply(effect, window, cx);
        }
        // A socket that came back: nothing folds a call's reply into the
        // mirror, so this is the only thing that can un-stale what the open
        // page asked for.
        let connected = self.state.connected();
        if connected && !self.was_connected {
            self.push_language_choice(window, cx);
            self.restart_updated_service(window, cx);
            self.refresh_firmware(false, window, cx);
            self.refresh_screen(true, window, cx);
        }
        self.was_connected = connected;
        // A deck the service has linked has been seen on this Mac, which is
        // what owner decision 7 waits for before anything is added (first
        // run's Add These, the Service page's Start at login).
        if self.linked() && !self.prefs(cx).deck_seen() {
            self.update_prefs(cx, Prefs::set_deck_seen);
        }
        self.on_deck_event(window, cx);
        self.on_faces_event(window, cx);
        self.on_integrations_event(window, cx);
        self.on_voice_event(window, cx);
        self.on_service_event(window, cx);
        self.on_firstrun_event(window, cx);
        if self.language() != previous_language {
            self.relocalize(window, cx);
        }
        self.update_menu(window, cx);
        cx.notify();
    }

    /// Every [`HostEffect`]. A missing arm here would not compile, and a
    /// mirror that quietly stopped folding is exactly what `StateGet`
    /// exists to stop.
    fn apply(&mut self, effect: HostEffect, window: &mut Window, cx: &mut Context<Self>) {
        match effect {
            // The speech-recognition sheet (§7.8). The Voice page is where
            // the permission is shown with what can honestly be done about
            // it, so the toast takes the owner there rather than raising a
            // sheet with a button that cannot prompt (§10.4).
            HostEffect::RaisePermissionSheet(SheetKind::Speech) => {
                self.go(Screen::Voice, window, cx);
                self.toast(
                    self.tr("Voice needs the speech-recognition permission"),
                    window,
                    cx,
                );
            }
            HostEffect::Toast(message) => self.toast(message, window, cx),
            // The `ApiClient` owns the reconnect and is already counting the
            // same back-off down; the strip says so without a second timer.
            HostEffect::Reconnect(_) => {}
            HostEffect::StateGet => self.resync(true, window, cx),
        }
    }

    pub(crate) fn toast(
        &self,
        message: impl Into<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message: String = message.into();
        window.push_notification(Notification::new().message(message).w(px(360.)), cx);
    }

    /// `state.get`, and its reply posted back onto the bus as the snapshot
    /// that closes the resync. The reducer is pure, so this round trip is
    /// the only way a mirror with a hole in it gets whole again.
    /// `announce` is true for the first ask of a hole and false for the
    /// retries below it: a notification every [`RESYNC_RETRY`] would be worse
    /// than the one that already said it.
    fn resync(&mut self, announce: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(reply) = self.state.send(&UiAction::StateGet) else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let installed = match reply.await {
                Ok(result) => ApiEvent::snapshot_reply(&result)
                    .is_some_and(|event| events::post(AppEvent::Api(event))),
                Err(_) => false,
            };
            if installed {
                return;
            }
            if announce {
                this.update_in(cx, |this, window, cx| {
                    this.toast(this.tr("Could not re-read the service's state"), window, cx);
                })
                .ok();
            }
            // Ask again. Nothing else will: see `RESYNC_RETRY`.
            cx.background_executor().timer(RESYNC_RETRY).await;
            this.update_in(cx, |this, window, cx| {
                if this.state.resyncing() {
                    this.resync(false, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Send a UI action and say so if it failed. Never awaits on this
    /// thread: `send` is a queue push that hands back a `oneshot::Receiver`,
    /// and the wait for it happens in `cx.spawn` on gpui's own executor.
    ///
    /// What leaves is what this run may ask for ([`app::Launch::outgoing`]):
    /// only the installed app has the service look for firmware.
    pub(crate) fn act(&mut self, action: UiAction, window: &mut Window, cx: &mut Context<Self>) {
        let action = app::launch().outgoing(action);
        let what = action_name(&action);
        let language = self.language();
        let Some(reply) = self.state.send(&action) else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            // `Reply` folds a dropped sender into `Disconnected`, so the
            // socket going away between the click and the reply arrives here
            // as an ordinary client error rather than a second layer.
            if let Err(e) = reply.await {
                let complaint =
                    language.format("{0} failed: {1}", &[language.text(what), &e.to_string()]);
                this.update_in(cx, |this, window, cx| this.toast(complaint, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Send an action **whose reply this window reads**, and hand the raw
    /// result back on the UI thread.
    ///
    /// A page that shows a call's answer works this way: nothing folds an
    /// `integrations.status` or a `care.footprint` into the mirror, so the
    /// only place its answer can live is the pane that asked. `then` runs
    /// with the `Shell` borrowed mutably, which is what lets it put the
    /// answer there.
    ///
    /// A detached state (no client, which is a test or a synthetic harness
    /// run and not the app) answers `Disconnected` **synchronously**, so a
    /// page can never be left with a spinner that nothing will ever stop.
    ///
    /// What leaves is what this run may ask for, as in [`Shell::act`]: the
    /// firmware read every connection makes is a `firmware.status` anywhere
    /// but in the installed app, and its reply has the same shape.
    pub(crate) fn ask(
        &mut self,
        action: UiAction,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Shell, Result<Value, ClientError>, &mut Window, &mut Context<Shell>)
            + 'static,
    ) {
        let action = app::launch().outgoing(action);
        let Some(reply) = self.state.send(&action) else {
            then(self, Err(ClientError::Disconnected), window, cx);
            cx.notify();
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = reply.await;
            this.update_in(cx, move |this, window, cx| {
                then(this, result, window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- the language ----------------------------------------------------

    /// The owner picked a language on this Mac (first run's popup). With the
    /// service there it is the deck's setting at once, and the window
    /// follows when the service says it took it; without one, the window
    /// speaks it now and hands it over when the service answers.
    pub(crate) fn choose_language(
        &mut self,
        language: Language,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.connected() {
            self.act(
                UiAction::DeviceSet {
                    key: SetKey::Language,
                    value: language as u16,
                },
                window,
                cx,
            );
        } else {
            self.set_language_choice(Some(language), window, cx);
        }
    }

    /// The first thing a connection does: give the service the language
    /// picked on this Mac. Nothing is sent when the service already speaks
    /// it. The choice is dropped once the service has taken it, so from then
    /// on the window speaks the deck's setting; a refused write keeps it for
    /// the next connection.
    fn push_language_choice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(choice) = self.language_choice else {
            return;
        };
        if spoken(None, self.state.mirror().settings.language) == choice {
            self.set_language_choice(None, window, cx);
            return;
        }
        let action = UiAction::DeviceSet {
            key: SetKey::Language,
            value: choice as u16,
        };
        self.ask(action, window, cx, move |this, result, window, cx| {
            // A second pick while this one was on its way is the one that
            // stays.
            if result.is_ok() && this.language_choice == Some(choice) {
                this.set_language_choice(None, window, cx);
            }
        });
    }

    /// Hold a pick for the service, or let go of one it has taken — here and
    /// in the preference, so a pick made before any service answered is
    /// still the one spoken after a quit.
    fn set_language_choice(
        &mut self,
        choice: Option<Language>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let before = self.language();
        self.language_choice = choice;
        cx.update_global::<Prefs, _>(|prefs, _| prefs.set_language(choice));
        if self.language() != before {
            self.relocalize(window, cx);
        }
        cx.notify();
    }

    /// The window's words after a language change: the app menu, then
    /// whatever each page keeps outside the render (text-field
    /// placeholders). The status item relabels itself on its next update.
    fn relocalize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        app::apply_menu_language(self.language(), cx);
        self.localize_pages(window, cx);
    }

    // ---- the pages, through their contract -------------------------------

    /// Open a screen, and ask for what it draws if it has nothing yet.
    pub(crate) fn go(&mut self, screen: Screen, window: &mut Window, cx: &mut Context<Self>) {
        if screen != self.screen {
            self.page_scroll.set_offset(gpui::point(px(0.), px(0.)));
        }
        self.screen = screen;
        self.refresh_screen(false, window, cx);
        cx.notify();
    }

    /// Ask the page that is up for what it draws: first run while it runs,
    /// else the open screen's page. `force` asks even when it already has an
    /// answer, which a reconnect needs, and so does a live harness scene.
    pub(crate) fn refresh_screen(
        &mut self,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.first_run.active() {
            self.refresh_firstrun(force, window, cx);
            return;
        }
        match self.screen {
            Screen::Deck => self.refresh_deck(force, window, cx),
            Screen::FacesKeys => self.refresh_faces(force, window, cx),
            Screen::Integrations => self.refresh_integrations(force, window, cx),
            Screen::Voice => self.refresh_voice(force, window, cx),
            Screen::Service | Screen::Footprint | Screen::LocalApi => {
                self.refresh_service(force, window, cx);
            }
            Screen::Kit | Screen::MenuPreview => {}
        }
    }

    /// Each page, once a second.
    fn tick_pages(&mut self, beat: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.tick_deck(beat, window, cx);
        self.tick_faces(beat, window, cx);
        self.tick_integrations(beat, window, cx);
        self.tick_voice(beat, window, cx);
        self.tick_service(beat, window, cx);
        self.tick_firstrun(beat, window, cx);
    }

    /// Each page's words that live outside its render, in the window's
    /// language.
    pub(crate) fn localize_pages(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.localize_deck(window, cx);
        self.localize_faces(window, cx);
        self.localize_integrations(window, cx);
        self.localize_voice(window, cx);
        self.localize_service(window, cx);
        self.localize_firstrun(window, cx);
    }

    /// The open screen's page.
    fn render_page(&self, dim: bool, cx: &Context<Self>) -> AnyElement {
        match self.screen {
            Screen::Deck => self.render_deck(dim, cx),
            Screen::FacesKeys => self.render_faces(dim, cx),
            Screen::Integrations => self.render_integrations(dim, cx),
            Screen::Voice => self.render_voice(dim, cx),
            Screen::Service | Screen::Footprint | Screen::LocalApi => self.render_service(dim, cx),
            Screen::Kit => self.render_kit(cx),
            Screen::MenuPreview => self.render_menu_preview(cx),
        }
    }

    // ---- the dialogs -----------------------------------------------------

    /// Open a page's dialog by the id its page draws it under
    /// (`<p>_dialog`). It takes the place of any dialog that is up; the page
    /// that opened that one is not told, which is safe because the scrim
    /// leaves no control reachable but the dialog's own.
    pub(crate) fn open_dialog(&mut self, id: &'static str, window: &mut Window) {
        self.sheet = None;
        self.dialog = Some(Dialog::Page(id));
        self.dialog_opened(window);
    }

    /// Close whatever dialog is up, the way its Cancel or Done does: the
    /// page whose dialog it was is told (`<p>_dialog_closed`), and a sheet's
    /// going-ahead never runs.
    pub(crate) fn close_dialog(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Dialog::Page(id)) = self.dialog.take() {
            self.page_dialog_closed(id, cx);
        }
        self.sheet = None;
        cx.notify();
    }

    /// Ask before doing what cannot be taken back. `then` runs only when
    /// the going-ahead button is pressed; Cancel, Escape, Return and the
    /// scrim all drop it. A page dialog under it is closed, and its page
    /// told.
    pub(crate) fn ask_first(
        &mut self,
        sheet: Sheet,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Shell, &mut Window, &mut Context<Shell>) + 'static,
    ) {
        if let Some(Dialog::Page(id)) = self.dialog.take() {
            self.page_dialog_closed(id, cx);
        }
        self.sheet = Some((sheet, Box::new(then)));
        self.dialog = Some(Dialog::Sheet);
        self.dialog_opened(window);
        cx.notify();
    }

    /// The sheet's going-ahead button.
    fn go_ahead(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, then)) = self.sheet.take() else {
            return;
        };
        self.dialog = None;
        then(self, window, cx);
        cx.notify();
    }

    /// A dialog has just appeared: give it the keyboard (Escape) and replay
    /// its entrance. The redraw is asked for here, because `focus` asks for
    /// none when the layer already had the keyboard, and a page opening a
    /// dialog has no context to notify.
    fn dialog_opened(&mut self, window: &mut Window) {
        self.dialog_seq = self.dialog_seq.wrapping_add(1);
        window.focus(&self.dialog_focus);
        window.refresh();
    }

    /// Escape, or a click on the scrim.
    fn dismiss_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some_and(Dialog::dismissable) {
            self.close_dialog(window, cx);
        }
    }

    /// Tab (`forward`) or Shift-Tab while a dialog is up: the next or the
    /// previous of the dialog's own controls, round and round. The window's
    /// order would go on past the dialog's last control to the page under
    /// the scrim, whose controls a dialog is there to keep out of reach.
    fn tab_in_dialog(&self, forward: bool, window: &mut Window, cx: &App) {
        let stops = self.dialog_stops(window, cx);
        let at = window
            .focused(cx)
            .and_then(|focused| stops.iter().position(|stop| *stop == focused));
        match cycle(at, stops.len(), forward) {
            Some(next) => window.focus(&stops[next]),
            None => window.focus(&self.dialog_focus),
        }
    }

    /// The dialog's controls in Tab order. The dialog is drawn after
    /// everything under it, so its controls follow its own focus handle in
    /// the window's order, one after another: they are found by walking on
    /// from there until the walk leaves the dialog. The keyboard is put
    /// back where it was.
    fn dialog_stops(&self, window: &mut Window, cx: &App) -> Vec<FocusHandle> {
        let was = window.focused(cx);
        window.focus(&self.dialog_focus);
        let mut stops: Vec<FocusHandle> = Vec::new();
        loop {
            window.focus_next();
            match window.focused(cx) {
                Some(next)
                    if next != self.dialog_focus
                        && self.dialog_focus.contains(&next, window)
                        && !stops.contains(&next) =>
                {
                    stops.push(next);
                }
                _ => break,
            }
        }
        match was {
            Some(was) => window.focus(&was),
            None => window.blur(),
        }
        stops
    }

    /// Put a keyboard that is nowhere back home — on the dialog that is up,
    /// or on the window's root — once this frame is drawn. Nowhere is no
    /// focus at all, or a focus on something the frame no longer draws: a
    /// control that went while it had the keyboard (its page was left, it
    /// was swapped for another), a menu that closed. Tab does nothing from
    /// there, because `Root` binds it inside the window and nowhere is
    /// outside. The check waits for the frame because only a drawn frame
    /// says what is still there.
    fn keep_keyboard(&self, window: &mut Window) {
        let root = self.root_focus.clone();
        let home = if self.dialog.is_some() {
            self.dialog_focus.clone()
        } else {
            self.root_focus.clone()
        };
        window.on_next_frame(move |window, cx| {
            let held = window
                .focused(cx)
                .is_some_and(|focused| root.contains(&focused, window));
            if !held {
                window.focus(&home);
            }
        });
    }

    /// The page dialog `id`, from whichever page draws it.
    fn page_dialog(&self, id: &'static str, cx: &Context<Self>) -> Option<(AnyElement, f32)> {
        self.deck_dialog(id, cx)
            .or_else(|| self.faces_dialog(id, cx))
            .or_else(|| self.integrations_dialog(id, cx))
            .or_else(|| self.voice_dialog(id, cx))
            .or_else(|| self.service_dialog(id, cx))
            .or_else(|| self.firstrun_dialog(id, cx))
    }

    /// Tell every page the dialog `id` went; the one that drew it cleans up.
    fn page_dialog_closed(&mut self, id: &'static str, cx: &mut Context<Self>) {
        self.deck_dialog_closed(id, cx);
        self.faces_dialog_closed(id, cx);
        self.integrations_dialog_closed(id, cx);
        self.voice_dialog_closed(id, cx);
        self.service_dialog_closed(id, cx);
        self.firstrun_dialog_closed(id, cx);
    }

    /// The shell's own question: its title, its sentence, **Cancel** (the
    /// default, so the primary) and the button that goes ahead (the danger
    /// kind when what it does cannot be taken back).
    fn render_sheet(&self, cx: &Context<Self>) -> Option<(AnyElement, f32)> {
        let (sheet, _) = self.sheet.as_ref()?;
        let p = *palette(cx);
        let go_ahead = Btn::new("sheet-confirm", sheet.confirm.clone())
            .on_click(cx.listener(|this, _, window, cx| this.go_ahead(window, cx)));
        let go_ahead = if sheet.danger {
            go_ahead.danger()
        } else {
            go_ahead
        };
        let body = v_flex()
            .p(px(20.))
            .gap(px(4.))
            .child(div().ts(13., 18.).semibold().child(sheet.title.clone()))
            .child(
                div()
                    .ts(13., 18.)
                    .text_color(p.text2)
                    .child(para(sheet.body.clone())),
            )
            .child(
                h_flex()
                    .mt(px(16.))
                    .gap(px(8.))
                    .justify_end()
                    .child(
                        Btn::new("sheet-cancel", self.tr("Cancel"))
                            .primary()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close_dialog(window, cx);
                            })),
                    )
                    .child(go_ahead),
            );
        Some((body.into_any_element(), SHEET_WIDTH))
    }

    /// The layer a dialog sits on: the scrim, the card, and the keys that
    /// close it.
    fn render_dialog_layer(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let dialog = self.dialog?;
        let p = *palette(cx);
        let (body, width) = match dialog {
            Dialog::Sheet => self.render_sheet(cx)?,
            Dialog::Page(id) => self.page_dialog(id, cx)?,
        };
        let seq = self.dialog_seq;
        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .track_focus(&self.dialog_focus)
                .key_context(DIALOG_KEYS)
                .on_action(cx.listener(|this, _: &NextControl, window, cx| {
                    this.tab_in_dialog(true, window, cx);
                }))
                .on_action(cx.listener(|this, _: &PreviousControl, window, cx| {
                    this.tab_in_dialog(false, window, cx);
                }))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() {
                        "escape" => {
                            cx.stop_propagation();
                            this.dismiss_dialog(window, cx);
                        }
                        // Cancel is the sheet's default button.
                        "enter" if this.dialog == Some(Dialog::Sheet) => {
                            cx.stop_propagation();
                            this.close_dialog(window, cx);
                        }
                        _ => {}
                    }
                }))
                .child(
                    div()
                        .id("scrim")
                        .absolute()
                        .inset_0()
                        .bg(p.scrim)
                        .occlude()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.dismiss_dialog(window, cx);
                        })),
                )
                .child(
                    // A card over the dimmed window: the surface, its rule
                    // and the one shadow the window has.
                    v_flex()
                        .id("dialog")
                        .occlude()
                        .w(px(width))
                        .max_h(relative(0.88))
                        .rounded(px(12.))
                        .bg(p.surf)
                        .border_1()
                        .border_color(p.line)
                        .shadow_lg()
                        .text_color(p.text)
                        .child(body)
                        .with_animation(
                            ElementId::NamedInteger("dialog-in".into(), seq),
                            Animation::new(Duration::from_millis(180))
                                .with_easing(ease_out_quint()),
                            |this, delta| this.opacity(delta).mt(px(8. * (1. - delta))),
                        ),
                )
                .into_any_element(),
        )
    }

    // ---- the frame's helpers ---------------------------------------------

    /// The band across the window's top ([`TITLEBAR_HEIGHT`]): drawn first,
    /// so it lies behind both layouts and takes up no room, and
    /// double-clicking it does what double-clicking a title bar does on this
    /// Mac. Nothing laid out over it blocks the pointer, so a double click
    /// anywhere in the top 28 px reaches it. A control a layout puts in
    /// that band (first run's language popup) occludes it, or a double click
    /// on the control would zoom the window as well.
    fn titlebar_band(&self) -> Stateful<Div> {
        div()
            .id("titlebar")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(TITLEBAR_HEIGHT))
            .on_click(|event, window, _| {
                if event.click_count() == 2 {
                    window.titlebar_double_click();
                }
            })
    }

    /// Kick the installed service's launch agent.
    ///
    /// Only the installed app, run for real, does. The label is the owner's
    /// own agent's whichever copy names it, so a build outside the bundle, a
    /// harness walk or a window on a test service would be restarting his
    /// service: those runs do nothing here, and the reason goes to stderr
    /// ([`app::Launch::hands_off`]). A page that would rather not offer the
    /// button in such a run asks `app::launch().installed()`.
    pub(crate) fn start_service(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(why) = app::launch().hands_off() {
            eprintln!("openwalkietalkie-app: not starting the service: {why}");
            return;
        }
        self.toast(self.tr("Starting the service…"), window, cx);
        // The one piece of I/O this crate does itself, and it goes where all
        // I/O goes: the bridge's runtime, never the UI thread.
        let done = app::start_service(&cx.global::<app::Tokio>().0);
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Err(why)) = done.recv().await {
                this.update_in(cx, |this, window, cx| {
                    this.toast(
                        this.fmt("Could not start the service: {0}", &[&why]),
                        window,
                        cx,
                    );
                })
                .ok();
            }
        })
        .detach();
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *palette(cx);
        // The sidebar, its rule included, comes off the window's width.
        self.content_width = f32::from(window.viewport_size().width) - SIDEBAR_WIDTH;
        // Disconnected: the last mirror is greyed, not blanked. What the
        // service last said is worth more than an empty window — but every
        // control on it would fail now, so they are off and the page is
        // dimmed to say why.
        let dim = !self.state.connected();
        // First run is the whole window, with no sidebar; everything else
        // is the frame around the open page. The main column runs from the
        // window's top edge: the page's own padding keeps it clear of the
        // band.
        let body = if self.first_run.active() {
            self.render_first_run(cx)
        } else if self.screen == Screen::MenuPreview {
            // The menu-bar board is the status item and its open menu at the
            // window's top-left, nothing else: no sidebar to reach over.
            div().size_full().child(self.render_page(dim, cx)).into_any_element()
        } else {
            h_flex()
                .size_full()
                .child(self.render_sidebar(cx))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .children(self.render_strips(cx))
                        .child(div().flex_1().min_h_0().child(self.render_page(dim, cx))),
                )
                .into_any_element()
        };
        self.keep_keyboard(window);
        // The window's type: Geist 13/18, with Chinese and Japanese drawn in
        // the window's language's own family (`style::ui_font`). The root is
        // the keyboard's home: a press on nothing in particular puts the
        // keyboard there too, so a ring left on a control goes.
        div()
            .size_full()
            .relative()
            .track_focus(&self.root_focus)
            .font(style::ui_font(self.language()))
            .ts(13., 18.)
            .text_color(p.text)
            .child(self.titlebar_band())
            .child(body)
            .children(self.render_dialog_layer(cx))
            // `Root` owns the notifications but does not draw them: the
            // view that raises them renders their layer.
            .children(Root::render_notification_layer(window, cx))
    }
}

/// The language a window speaks: the pick waiting for the service, if any,
/// else the deck's own setting as the service last reported it.
pub(crate) fn spoken(choice: Option<Language>, setting: u16) -> Language {
    choice.unwrap_or_else(|| Language::from_id(setting.min(255) as u8))
}

/// What a failed call is called in a toast.
fn action_name(action: &UiAction) -> &'static str {
    match action {
        UiAction::StateGet => "Re-read state",
        UiAction::IntegrationsStatus { .. } => "Read the integrations",
        UiAction::Integrations { .. } => "Write the integration",
        UiAction::TokenList => "List the tokens",
        UiAction::TokenCreate { .. } => "Create the token",
        UiAction::TokenRevoke { .. } => "Revoke the token",
        UiAction::TokenRotate { .. } => "Rotate the token",
        UiAction::PermStatus => "Read the permissions",
        UiAction::ServiceInfo => "Read the service",
        UiAction::ConfigGet => "Read the config",
        UiAction::ConfigSet { .. } => "Change the config",
        UiAction::ServiceStop => "Stop the service",
        UiAction::ServiceRestartWhenIdle => "Restart the updated service when idle",
        UiAction::ServiceRestart => "Restart the service",
        UiAction::LogTail { .. } => "Read the log",
        UiAction::VoiceEngines => "Read the engines",
        UiAction::VoiceTest { .. } => "Test the voice path",
        UiAction::PermRequest { .. } => "Ask for the permission",
        UiAction::DeviceSet { .. } => "Change the deck setting",
        UiAction::FirmwareStatus => "Read firmware update status",
        UiAction::FirmwareCheck { .. } => "Check firmware updates",
        UiAction::FirmwareInstall { .. } => "Install firmware",
        UiAction::SttStatus => "Read the recogniser",
        UiAction::SttInstall { .. } => "Download the model",
        UiAction::SttRemove => "Remove the model",
        UiAction::TtsStatus => "Read the voice",
        UiAction::TtsInstall { .. } => "Download the voice",
        UiAction::TtsRemove => "Remove the voice",
        UiAction::DeviceFacesSet { .. } => "Change the deck's faces",
        UiAction::DeviceMacroSet { .. } => "Change the macro keys",
        UiAction::ConfigPatch { .. } => "Change the config",
        UiAction::SourcesList => "Read the sources",
        UiAction::TestQuestion => "Send a test question",
        UiAction::TestVoice => "Start the voice test",
        UiAction::TestSay => "Speak a sample",
        UiAction::TestStatus => "Read the test result",
        UiAction::CareChecks => "Check the setup",
        UiAction::CareFootprint => "Read the footprint",
        UiAction::CareSupportFile { .. } => "Export the support file",
        UiAction::CareCacheClear { .. } => "Delete the firmware downloads",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_ui_action_has_a_name_a_toast_can_start_a_sentence_with() {
        for action in [
            UiAction::StateGet,
            UiAction::IntegrationsStatus { detect: true },
            UiAction::Integrations {
                verb: deck_gui_core::Verb::Install,
                target: "claude".into(),
                trust: false,
                dry_run: true,
            },
            UiAction::TokenList,
            UiAction::TokenCreate {
                name: "script".into(),
                scopes: vec!["observe".into()],
                expires: None,
            },
            UiAction::TokenRevoke {
                name: "script".into(),
            },
            UiAction::TokenRotate {
                name: "hooks".into(),
            },
            UiAction::PermStatus,
            UiAction::ServiceInfo,
            UiAction::ConfigGet,
            UiAction::ConfigSet {
                key: "api.port".into(),
                value: deck_gui_core::ConfigValue::Int(47_202),
            },
            UiAction::ServiceStop,
            UiAction::ServiceRestart,
            UiAction::LogTail {
                lines: 200,
                level: None,
            },
            UiAction::DeviceFacesSet { list: Vec::new() },
            UiAction::DeviceMacroSet {
                f: 3,
                layout: "app".into(),
                src: None,
                keys: Vec::new(),
            },
            UiAction::ConfigPatch {
                entries: vec![(
                    "usage.providers".into(),
                    deck_gui_core::ConfigValue::List(vec!["codex".into()]),
                )],
            },
            UiAction::SourcesList,
            UiAction::TestQuestion,
            UiAction::TestVoice,
            UiAction::TestSay,
            UiAction::TestStatus,
            UiAction::CareChecks,
            UiAction::CareFootprint,
            UiAction::CareSupportFile {
                to: "desktop".into(),
            },
            UiAction::CareCacheClear {
                what: "firmware".into(),
            },
        ] {
            let name = action_name(&action);
            assert!(!name.is_empty());
            assert!(
                name.chars().next().is_some_and(char::is_uppercase),
                "{name} starts a sentence in a toast"
            );
            // The toast reads it out in the window's language.
            assert!(
                deck_gui_core::i18n::has_entry(name),
                "{name} has no row"
            );
            // A name that reads as the value it carries would put a token
            // name — or worse — into a toast built from the method alone.
            assert!(
                !name.contains('{') && !name.contains('"'),
                "{name} is a phrase, not a payload"
            );
        }
    }

    #[test]
    fn the_window_opens_on_the_deck_unless_a_gate_asks_for_another_screen() {
        for (name, screen) in [
            ("deck", Screen::Deck),
            ("faces", Screen::FacesKeys),
            ("integrations", Screen::Integrations),
            ("voice", Screen::Voice),
            (" service ", Screen::Service),
            ("footprint", Screen::Footprint),
            ("local-api", Screen::LocalApi),
            ("kit", Screen::Kit),
            ("menu", Screen::MenuPreview),
            // The labels work too, in either case.
            ("Faces & Keys", Screen::FacesKeys),
            ("Integrations", Screen::Integrations),
            ("LOCAL API", Screen::LocalApi),
        ] {
            assert_eq!(Screen::parse(name), screen, "{name:?}");
        }
        for screen in Screen::ALL {
            assert_eq!(Screen::parse(screen.slug()), screen);
        }
        // Anything else is the Deck, which is where the window opens anyway
        // — a typo in an environment variable must not leave a blank window.
        // The console's screens are gone with it.
        for gone in ["usage", "welcome", "settings", ""] {
            assert_eq!(Screen::parse(gone), Screen::Deck, "{gone:?}");
        }
        assert_eq!(SCREEN_ENV, "OPENWALKIETALKIE_SCREEN");
    }

    #[test]
    fn the_sidebar_names_the_five_pages_and_a_sub_page_keeps_service_outlined() {
        let labels: Vec<&str> = Screen::NAV.iter().map(|s| s.label()).collect();
        assert_eq!(
            labels,
            vec!["Deck", "Faces & Keys", "Integrations", "Voice", "Service"]
        );
        for screen in Screen::ALL {
            let row = screen.nav();
            assert_eq!(
                row.is_some_and(|row| row == screen),
                Screen::NAV.contains(&screen),
                "{screen:?} is a row of its own exactly when it is in the sidebar"
            );
            if let Some(row) = row {
                assert!(Screen::NAV.contains(&row), "{screen:?} outlines {row:?}");
            }
        }
        assert_eq!(Screen::Footprint.nav(), Some(Screen::Service));
        assert_eq!(Screen::LocalApi.nav(), Some(Screen::Service));
        assert_eq!(Screen::Kit.nav(), None);
        assert_eq!(Screen::MenuPreview.nav(), None);
        let ids: Vec<&str> = Screen::ALL.iter().map(|s| s.id()).collect();
        let slugs: Vec<&str> = Screen::ALL.iter().map(|s| s.slug()).collect();
        for (i, (id, slug)) in ids.iter().zip(&slugs).enumerate() {
            assert!(id.starts_with("nav-"));
            assert!(!ids[i + 1..].contains(id), "{id} is used twice");
            assert!(!slugs[i + 1..].contains(slug), "{slug} is used twice");
        }
    }

    #[test]
    fn the_repaint_tick_is_the_granularity_the_countdowns_are_read_to() {
        assert_eq!(TICK, Duration::from_secs(1));
        assert!(RESYNC_RETRY > TICK, "a retry storm is worse than a wait");
    }

    #[test]
    fn a_language_picked_on_this_mac_is_spoken_until_the_service_has_taken_it() {
        // Nothing picked: the deck's setting, whatever the service said.
        assert_eq!(spoken(None, 0), Language::English);
        assert_eq!(spoken(None, 3), Language::Japanese);
        // An id this build has no language for reads as English.
        assert_eq!(spoken(None, 900), Language::English);
        // A pick waiting for the service wins over the setting it would
        // replace.
        assert_eq!(
            spoken(Some(Language::SimplifiedChinese), 0),
            Language::SimplifiedChinese
        );
        for language in Language::ALL {
            assert_eq!(spoken(None, language as u16), language);
        }
    }

    #[test]
    fn tab_goes_round_a_dialogs_controls_and_never_past_them() {
        // Three controls: on from each, back from each, round at the ends.
        assert_eq!(cycle(Some(0), 3, true), Some(1));
        assert_eq!(cycle(Some(2), 3, true), Some(0));
        assert_eq!(cycle(Some(0), 3, false), Some(2));
        assert_eq!(cycle(Some(1), 3, false), Some(0));
        // From the dialog itself (no control has the keyboard yet): the
        // first, or with Shift-Tab the last.
        assert_eq!(cycle(None, 3, true), Some(0));
        assert_eq!(cycle(None, 3, false), Some(2));
        // One control keeps it; a dialog with none keeps it on itself.
        assert_eq!(cycle(Some(0), 1, true), Some(0));
        assert_eq!(cycle(Some(0), 1, false), Some(0));
        assert_eq!(cycle(None, 0, true), None);
        // A place past the end (a control that went) starts again.
        assert_eq!(cycle(Some(5), 2, true), Some(0));
        assert_eq!(cycle(Some(5), 2, false), Some(1));
    }

    #[test]
    fn only_a_token_being_shown_outlives_escape_and_the_scrim() {
        assert!(Dialog::Sheet.dismissable());
        for id in ["plan", "link-details", "add-face", "firmware", "add-key"] {
            assert!(Dialog::Page(id).dismissable(), "{id}");
        }
        assert!(!Dialog::Page("token").dismissable());
    }

    #[test]
    fn every_word_the_shell_says_itself_has_a_row() {
        let source = include_str!("shell.rs");
        assert_eq!(crate::i18n::missing_rows(source), Vec::<String>::new());
        // Pinned, so a lookup the scanner stopped seeing fails here rather
        // than passing as a shell with less to translate. (Six since the
        // Mac's microphone stopped being a voice path: its toast is gone.)
        assert_eq!(crate::i18n::literals(source).len(), 6);
    }
}
