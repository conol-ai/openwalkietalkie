//! The window itself (§8.2's sidebar navigation): the mirror, the bus, the
//! sidebar, the strip that says the service is down, and the dialogs that
//! can arrive whichever screen is up.
//!
//! One view owns all of it. [`Shell`] holds the single [`HostState`], pumps
//! the single bus and is the only thing that calls the daemon, and each
//! screen is a pane it renders from its own small piece of view state
//! ([`crate::deck`], [`crate::integrations`], [`crate::service`]). That is
//! deliberate: three views each with a client would be three mirrors that
//! could disagree, and the reducer's whole point is that there is one.
//!
//! The view holds no state about the deck. Everything it draws comes out of
//! the mirror through [`crate::present`], and every button sends a
//! [`UiAction`] — `HostState::send` takes `&self` and cannot write to the
//! mirror, so a click's only effect on this window is the events the daemon
//! sends back. A window that answered its own optimistic `op.answer` would
//! show a card as answered that the daemon had refused.
//!
//! What it does keep is about the window and not about the deck: which
//! screen is up, which row is selected, what this GUI last answered (so
//! **Retry** can re-send exactly that, §4.8), what the admin screens last
//! asked the daemon for, and the confirmation request in front of the
//! operator.
//!
//! **The dialogs are the window's own**, drawn by [`Shell::render`] from that
//! state rather than handed to gpui-component's sheet and dialog layers. A
//! layer builder runs while this view is leased and cannot read it, so it had
//! to be given a copy of everything it showed; a dialog drawn here reads the
//! live state instead — the confirmation's Allow appears only while the card
//! still has an allow, and the token's plaintext stays in the one place it
//! is kept. Which dialog is up is derived, never stored: a confirmation
//! request outranks a token being shown, which outranks a plan.
//!
//! Every `render_*` takes `&Context<Self>` and not `&mut` on purpose: a
//! renderer that cannot borrow the context mutably cannot reach the mirror,
//! and the compiler keeps that promise instead of a comment.
use crate::app;
use crate::confirm::{self, Decision};
use crate::deck::DeckPane;
use crate::integrations::IntegrationsPane;
use crate::present;
use crate::service::ServicePane;
use crate::style::{self, palette, Palette, SANS};
use crate::tray::Tray;
use crate::ui::{self, ic, Btn, Severity, Type as _};
use crate::voice::VoicePane;
use crate::welcome::WelcomePane;
use deck_client::ClientError;
use deck_gui_core::tray::{Summary, TrayCommand};
use deck_gui_core::{
    events, ApiEvent, AppEvent, ConfirmRequest, EventReceiver, HostEffect, HostState, SheetKind,
    UiAction,
};
use deck_model::{AgentId, AgentState, Card};
use gpui::{
    div, ease_out_quint, prelude::*, px, relative, Animation, AnimationExt as _, AnyElement, App,
    Context, Div, ElementId, FocusHandle, KeyDownEvent, ScrollHandle, Stateful, Subscription, Task,
    Window,
};
use gpui_component::notification::Notification;
use gpui_component::{h_flex, v_flex, Root, WindowExt as _};
use serde_json::Value;
use std::time::Duration;

/// How often the window repaints itself with nothing new from the daemon.
/// The state ages and the hold countdown are the only things that move on
/// their own, and both are read to the second.
pub const TICK: Duration = Duration::from_secs(1);

/// How long the screen waits before asking for the snapshot again when a
/// `state.get` did not come back with one (an error, or `-32013 degraded`).
///
/// The reducer asks **once** per hole and then goes quiet — with a
/// `state.get` outstanding it issues no second `StateGet`, and while the
/// mirror is frozen every later event is another hole, so nothing on the
/// daemon's side can ever re-arm it. Re-asking is therefore the screen's job,
/// and without it one refused `state.get` freezes the window's mirror for as
/// long as it is open.
/// Repaint ticks between reads of the Claude channel rows while the
/// Integrations screen is open.
const CHANNEL_POLL_BEATS: u32 = 5;
const RESYNC_RETRY: Duration = Duration::from_secs(2);

/// The sidebar. Narrow enough that the Deck's roster and detail still split
/// the rest of the smallest window evenly (see the test at the foot of
/// `deck.rs`).
pub const SIDEBAR_WIDTH: f32 = 220.;

/// The transparent titlebar band across the top of the window. The traffic
/// lights sit in it at (14, 14), so their centres are on its midline, and
/// everything below it starts clear of them.
pub const TITLEBAR_HEIGHT: f32 = 38.;

/// The screens v1 ships (§8.2). Device, Settings, About and the full Voice
/// and Service screens are v1.1's; a sidebar row for a screen that does not
/// exist is a promise the window cannot keep, so they are not here.
///
/// **Welcome** is one of them and not a modal. §8.2 asks for a Welcome that
/// is re-openable, the sidebar is already how this window re-opens a screen,
/// and a first-run flow that cannot be re-read is the one an operator needs
/// most on the day something breaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Deck,
    Usage,
    Integrations,
    Voice,
    Service,
    Welcome,
}

impl Screen {
    pub const ALL: [Screen; 6] = [
        Screen::Deck,
        Screen::Usage,
        Screen::Integrations,
        Screen::Voice,
        Screen::Service,
        Screen::Welcome,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Screen::Deck => "Deck",
            Screen::Usage => "Usage",
            Screen::Integrations => "Integrations",
            Screen::Voice => "Voice",
            Screen::Service => "Service",
            Screen::Welcome => "Welcome",
        }
    }

    /// The `id` its sidebar row is drawn with.
    pub const fn id(self) -> &'static str {
        match self {
            Screen::Deck => "nav-deck",
            Screen::Usage => "nav-usage",
            Screen::Integrations => "nav-integrations",
            Screen::Voice => "nav-voice",
            Screen::Service => "nav-service",
            Screen::Welcome => "nav-welcome",
        }
    }

    /// The glyph its sidebar row leads with.
    pub const fn icon(self) -> &'static str {
        match self {
            Screen::Deck => ic::DIAL,
            Screen::Usage => ic::CLOCK,
            Screen::Integrations => ic::PLUG,
            Screen::Voice => ic::MICROPHONE,
            Screen::Service => ic::SERVER,
            Screen::Welcome => ic::MAGIC,
        }
    }

    /// A screen by its lower-cased label. Unknown names are the Deck, which
    /// is what the window opens on anyway.
    pub fn parse(name: &str) -> Screen {
        Screen::ALL
            .into_iter()
            .find(|s| s.label().eq_ignore_ascii_case(name.trim()))
            .unwrap_or(Screen::Deck)
    }
}

/// Which screen the window opens on. It is the Deck unless
/// [`SCREEN_ENV`] says otherwise, which is how a run gate proves that a
/// screen renders: `runtime_shaders` defers shader compilation to
/// window-open time, so only a screen that has actually been drawn has been
/// shown to work.
pub fn initial_screen() -> Screen {
    std::env::var(SCREEN_ENV)
        .ok()
        .map_or(Screen::Deck, |name| Screen::parse(&name))
}

/// See [`initial_screen`].
pub const SCREEN_ENV: &str = "OPENWALKIETALKIE_SCREEN";

/// A confirmation request (§4.2) and the words its dialog says, fixed when it
/// arrived. What a click *does* is decided against the live mirror in
/// [`Shell::decide`]; the words are the request as the operator first saw it,
/// so a card answered meanwhile does not rewrite the question under them.
pub(crate) struct Confirming {
    pub(crate) request: ConfirmRequest,
    summary: String,
    text: Option<String>,
}

/// The dialog in front of the window, derived from the state each one shows
/// ([`Shell::dialog`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dialog {
    Confirm,
    Token,
    Plan,
    /// The first-start question about a local recogniser (§7.6): asked
    /// once per Mac, answered into `[voice.stt] prompted`.
    Recognition,
}

pub struct Shell {
    pub(crate) state: HostState,
    updater: crate::updater::MacOsUpdater,
    pub(crate) update_restart_attempted: bool,
    pub(crate) firmware: crate::updates::FirmwarePane,
    pub(crate) screen: Screen,
    pub(crate) deck: DeckPane,
    pub(crate) integrations: IntegrationsPane,
    pub(crate) voice: VoicePane,
    pub(crate) service: ServicePane,
    pub(crate) welcome: WelcomePane,
    /// The request in the confirmation dialog. Kept here so a dismissal can
    /// be turned into a denial (§4.2) — and so a second
    /// `op.confirm_requested` can deny the one it replaces instead of
    /// dropping it.
    pub(crate) confirm: Option<Confirming>,
    /// The first-start recognition prompt is up.
    pub(crate) recognition_prompt: bool,
    /// …or was raised once in this run already, so a reconnect does not
    /// ask twice before the answer has landed in the config.
    pub(crate) recognition_asked: bool,
    /// Where Escape goes while a dialog is up.
    dialog_focus: FocusHandle,
    /// Bumped whenever a dialog opens, so its entrance plays again.
    dialog_seq: u64,
    /// The admin screens' scroll. One handle, because one screen is up at a
    /// time, and a screen opened from the sidebar starts at its top.
    pub(crate) page_scroll: ScrollHandle,
    /// The width right of the sidebar, as of this frame. Screens size their
    /// columns from it in pixels (see `ui::page`).
    pub(crate) content_width: f32,
    /// The connection as the last event left it, so a socket that came back
    /// can re-ask for what the open screen shows: nothing folds an
    /// `integrations.status` or a `service.info` into the mirror, so a
    /// reconnect leaves both screens quietly stale until something asks.
    was_connected: bool,
    /// §4.1's degrade, as the tray last saw it. A reason that has not
    /// changed raises nothing: the daemon re-probes the port every 30 s and
    /// re-emits, and a notification per probe is not a notification.
    announced_degrade: Option<String>,
    /// The bus pump, the repaint tick, the two text boxes' Return keys and
    /// the appearance watch. Held because a dropped `Task`/`Subscription` is
    /// a cancelled one.
    _pump: Task<()>,
    _tick: Task<()>,
    _keys: Vec<Subscription>,
}

impl Shell {
    pub fn new(
        state: HostState,
        bus: EventReceiver,
        screen: Screen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (deck, deck_keys) = DeckPane::new(window, cx);
        let (service, service_keys) = ServicePane::new(window, cx);
        let (voice, voice_keys) = VoicePane::new(window, cx);
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
            let mut beats = 0u32;
            loop {
                cx.background_executor().timer(TICK).await;
                beats = beats.wrapping_add(1);
                let polled = this.update_in(cx, |this, window, cx| {
                    // The channel rows are cheap to read (no CLI probes), so
                    // they follow sessions starting and helpers retiring.
                    if beats % CHANNEL_POLL_BEATS == 0 {
                        this.poll_claude_channels(window, cx);
                    }
                    cx.notify();
                });
                if polled.is_err() {
                    break;
                }
            }
        });
        // Light and dark follow the Mac (or `OPENWALKIETALKIE_APPEARANCE`,
        // which pins the Mac's answer for this app): the palette is
        // re-derived and re-installed on every change, never patched.
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            let frosted = palette(cx).frosted;
            style::install(
                style::Palette::for_appearance(window.appearance(), frosted),
                cx,
            );
            window.refresh();
        });
        // The screen the window opens on asks for what it draws — from
        // *after* construction, because `go` needs the `Shell` that is still
        // being built here.
        cx.spawn_in(window, async move |this, cx| {
            this.update_in(cx, move |this, window, cx| this.go(screen, window, cx))
                .ok();
        })
        .detach();
        let mut keys = deck_keys;
        keys.extend(service_keys);
        keys.extend(voice_keys);
        keys.push(appearance);
        keys.push(cx.on_release(|this, cx| this.deck.images.clear(cx)));
        Shell {
            updater: crate::updater::MacOsUpdater::new(),
            update_restart_attempted: false,
            firmware: crate::updates::FirmwarePane::default(),
            state,
            screen,
            deck,
            integrations: IntegrationsPane::new(window, cx),
            voice,
            service,
            welcome: WelcomePane::default(),
            confirm: None,
            recognition_prompt: false,
            recognition_asked: false,
            dialog_focus: cx.focus_handle(),
            dialog_seq: 0,
            page_scroll: ScrollHandle::new(),
            content_width: crate::app::WINDOW_WIDTH - SIDEBAR_WIDTH,
            was_connected: false,
            announced_degrade: None,
            _pump: pump,
            _tick: tick,
            _keys: keys,
        }
    }

    /// One event from the bus: fold it, then do what the reducer asked.
    fn on_app_event(&mut self, event: AppEvent, window: &mut Window, cx: &mut Context<Self>) {
        // A tray click is an action and the reducer issues none, so it is
        // taken here. It still goes through the bus rather than straight
        // into a call, because `muda` runs its handler on the menu's own
        // thread and the bus is the one path into this view from off it.
        if let AppEvent::Tray(command) = event {
            self.on_tray(command, window, cx);
            return;
        }
        for effect in self.state.handle_event(event) {
            self.apply(effect, window, cx);
        }
        // A selection the roster no longer has is not a selection.
        if self
            .deck
            .selected
            .as_ref()
            .is_some_and(|id| !self.state.mirror().agents.contains_key(id))
        {
            self.deck.selected = None;
        }
        // A socket that came back: the two admin screens have no events to
        // fold, so this is the only thing that can un-stale them.
        let connected = self.state.connected();
        if connected && !self.was_connected {
            // The tray's **Pause hooks** tick is `[hooks] enabled`, and the
            // tray is there whether or not the Service screen ever is. One
            // `config.get` per connection is what keeps that item from being
            // permanently disabled in tray-only mode.
            self.read_hooks(window, cx);
            self.restart_updated_service(window, cx);
            self.refresh_firmware(false, window, cx);
            match self.screen {
                Screen::Deck | Screen::Usage => {}
                Screen::Integrations => self.refresh_integrations(true, window, cx),
                Screen::Voice => self.refresh_voice(true, window, cx),
                Screen::Service => self.refresh_service(true, window, cx),
                Screen::Welcome => self.refresh_welcome(true, window, cx),
            }
        }
        self.refresh_images(window, cx);
        self.was_connected = connected;
        self.update_tray(window, cx);
        cx.notify();
    }

    /// Redraw the status item, and raise §8.2's one-time alert when the
    /// service has just gone degraded.
    ///
    /// The alert is an **in-window** notification and not a Notification
    /// Centre banner, and the difference is worth naming: a system
    /// notification needs `UNUserNotificationCenter`, which needs the signed
    /// bundle of §10.1 — week 7's gate. What is here reaches the operator
    /// when the window is open and the tooltip carries it when it is not.
    fn update_tray(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let summary = self.tray_summary();
        if cx.has_global::<Tray>() {
            cx.update_global::<Tray, ()>(|tray, _| tray.update(&summary));
        }
        if summary.degraded == self.announced_degrade {
            return;
        }
        self.announced_degrade = summary.degraded.clone();
        let Some(detail) = summary.detail.clone().filter(|_| summary.wants_attention()) else {
            return;
        };
        let entity = cx.entity();
        window.push_notification(
            Notification::warning(detail)
                .title("The service is degraded")
                .autohide(false)
                .w(px(380.))
                .on_click(move |_, window, cx| {
                    entity.update(cx, |this, cx| this.go(Screen::Integrations, window, cx));
                }),
            cx,
        );
    }

    /// `config.get`, for the one key the tray needs. It lands in the Service
    /// screen's own config, because that is where `config.get` already
    /// lives: two copies of one answer could disagree.
    fn read_hooks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask(UiAction::ConfigGet, window, cx, |this, result, window, cx| {
            if let Ok(value) = result {
                this.service
                    .set_config(deck_gui_core::admin::ApiConfig::parse(&value));
                // The first start's one question (§7.6): asked when the
                // config says it never was, once per run, and never on top
                // of another dialog.
                let stt = deck_gui_core::voice::SttConfig::parse(&value);
                if !stt.prompted && !this.recognition_asked && this.dialog().is_none() {
                    this.recognition_asked = true;
                    this.recognition_prompt = true;
                    this.refresh_stt(window, cx);
                    this.dialog_opened(window);
                }
            }
        });
    }

    /// What the tray draws: the mirror, the socket, and the one `[hooks]`
    /// key `config.get` answered.
    pub(crate) fn tray_summary(&self) -> Summary {
        Summary::of(
            self.state.mirror(),
            self.state.connected(),
            self.service.hooks_enabled(),
        )
    }

    /// A click on the status item (§8.2's tray row).
    ///
    /// Every item that changes anything becomes the same `UiAction` a button
    /// on a screen sends, so there is one path to the daemon and one writer
    /// of the mirror. The two that change nothing on the daemon —
    /// **Open** and **Quit** — are the window's own, and Quit ends this app
    /// alone: the service is a launchd agent and keeps running.
    fn on_tray(&mut self, command: TrayCommand, window: &mut Window, cx: &mut Context<Self>) {
        let summary = self.tray_summary();
        match command {
            TrayCommand::Open => {
                app::show(cx);
                window.activate_window();
            }
            TrayCommand::Mute => self.act(summary.mute_action(), window, cx),
            TrayCommand::PauseHooks => match summary.pause_hooks_action() {
                Some(action) => self.act(action, window, cx),
                // The tick is disabled until `config.get` has answered, so
                // this is only reachable by a click that raced the read.
                None => self.toast(
                    "The service has not said whether hooks are on yet.",
                    window,
                    cx,
                ),
            },
            TrayCommand::CheckUpdates => self.check_updates(window, cx),
            TrayCommand::Quit => cx.quit(),
            TrayCommand::StopService => self.act(UiAction::ServiceStop, window, cx),
        }
        cx.notify();
    }

    /// §8.2's **Check for Updates…**.
    ///
    /// Sparkle owns signed download, installation, and relaunch.
    pub(crate) fn check_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.updater.check_for_updates() {
            Ok(crate::updater::UpdateCheck::Started) => {}
            Ok(crate::updater::UpdateCheck::Busy) => self.toast("An app update is already in progress.", window, cx),
            Err(reason) => self.toast(reason, window, cx),
        }
        self.refresh_firmware(true, window, cx);
    }

    /// Every [`HostEffect`] — all **six** of them. §8.1's prose names five
    /// and then requires the sixth in its own text ("a gap triggers
    /// `state.get`"); a five-arm match here would not compile, and a mirror
    /// that quietly stopped folding is exactly what `StateGet` exists to
    /// stop.
    fn apply(&mut self, effect: HostEffect, window: &mut Window, cx: &mut Context<Self>) {
        match effect {
            HostEffect::ShowWindow => {
                cx.activate(true);
                window.activate_window();
            }
            HostEffect::RaisePermissionSheet(SheetKind::Confirm(request)) => {
                self.raise_confirm(request, window, cx);
            }
            // The microphone and speech sheets (§7.8). The Voice screen is
            // where both are shown with what can honestly be done about
            // them, so the toast takes the operator there rather than
            // raising a sheet with a button that cannot prompt (§10.4).
            HostEffect::RaisePermissionSheet(SheetKind::Microphone) => {
                self.go(Screen::Voice, window, cx);
                self.toast("Voice needs the microphone permission", window, cx);
            }
            HostEffect::RaisePermissionSheet(SheetKind::Speech) => {
                self.go(Screen::Voice, window, cx);
                self.toast("Voice needs the speech-recognition permission", window, cx);
            }
            HostEffect::Toast(message) => self.toast(message, window, cx),
            // The `ApiClient` owns the reconnect and is already counting the
            // same back-off down; the strip says so without a second timer.
            HostEffect::Reconnect(_) => {}
            HostEffect::OpenUrl(url) => cx.open_url(&url),
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
                    this.toast("Could not re-read the service's state", window, cx);
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

    /// Send a UI action and report what came back. Never awaits on this
    /// thread: `send` is a queue push that hands back a `oneshot::Receiver`,
    /// and the wait for it happens in `cx.spawn` on gpui's own executor.
    pub(crate) fn act(&mut self, action: UiAction, window: &mut Window, cx: &mut Context<Self>) {
        if let UiAction::Answer {
            agent,
            card,
            choice,
            text,
        } = &action
        {
            // Remembered for Retry (§4.8): an answer that failed can only be
            // re-sent by whoever knows what it was, and `card.failed` does
            // not carry the choice. Recorded against the card the mirror
            // holds *now*, whose `created` stamp is what tells a later card
            // under the same id apart from this one.
            if let Some(live) = self.card_of(Some(agent)).filter(|c| c.id == *card) {
                self.deck.remember(agent, &live, *choice, text.clone());
            }
        }
        let what = action_name(&action);
        let Some(reply) = self.state.send(&action) else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let complaint = match reply.await {
                // §4.7: `op.answer`/`op.order` answer *after* delivery, so a
                // `delivered:false` is the daemon's own report and not an
                // error — and this is the only place it is ever said.
                Ok(result) => undelivered(&result).map(|why| format!("{what}: {why}")),
                // `Reply` folds a dropped sender into `Disconnected`, so the
                // socket going away between the click and the reply arrives
                // here as an ordinary client error rather than a second layer.
                Err(e) => Some(format!("{what} failed: {e}")),
            };
            if let Some(complaint) = complaint {
                this.update_in(cx, |this, window, cx| this.toast(complaint, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Send an action **whose reply this window reads**, and hand the raw
    /// result back on the UI thread.
    ///
    /// The Integrations and Service screens work this way and the Deck
    /// screen does not, because their calls produce no events: nothing folds
    /// an `integrations.status` or a `log.tail` into the mirror, so the only
    /// place their answers can live is the pane that asked. `then` runs with
    /// the `Shell` borrowed mutably, which is what lets it put the answer
    /// there.
    ///
    /// A detached state (no client, which is a test and not the app) answers
    /// `Disconnected` **synchronously**, so a screen can never be left with
    /// a spinner that nothing will ever stop.
    pub(crate) fn ask(
        &mut self,
        action: UiAction,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Shell, Result<Value, ClientError>, &mut Window, &mut Context<Shell>)
            + 'static,
    ) {
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

    // ---- the dialogs ---------------------------------------------------

    /// The dialog in front of the window, if any. A confirmation outranks
    /// everything: it is an agent blocked on the operator. A token being
    /// shown outranks a plan, because closing it is the end of the value.
    pub(crate) fn dialog(&self) -> Option<Dialog> {
        if self.confirm.is_some() {
            Some(Dialog::Confirm)
        } else if self.integrations.showing_token() {
            Some(Dialog::Token)
        } else if self.integrations.plan_open() {
            Some(Dialog::Plan)
        } else if self.recognition_prompt {
            Some(Dialog::Recognition)
        } else {
            None
        }
    }

    /// A dialog has just appeared: give it the keyboard (Escape) and replay
    /// its entrance.
    pub(crate) fn dialog_opened(&mut self, window: &mut Window) {
        self.dialog_seq = self.dialog_seq.wrapping_add(1);
        window.focus(&self.dialog_focus);
    }

    /// Escape, or a click on the scrim. A confirmation **denies** (§4.2); a
    /// plan is cancelled; a token being shown stays until its Done is
    /// pressed, because a stray Escape must not destroy the only copy.
    fn dismiss_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.dialog() {
            Some(Dialog::Confirm) => self.decide(Decision::Deny, window, cx),
            Some(Dialog::Plan) => self.cancel_plan(cx),
            // Waving the question away is "keep Apple's", recorded as
            // answered: a prompt that came back at every launch would be a
            // nag, not a question.
            Some(Dialog::Recognition) => self.answer_recognition(crate::voice::Opt::Keep, window, cx),
            Some(Dialog::Token) | None => {}
        }
    }

    // ---- the confirmation (§4.2) ---------------------------------------

    pub(crate) fn raise_confirm(
        &mut self,
        request: ConfirmRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A second request replaces the dialog, so the one it replaces is
        // dismissed — and a dismissal denies (§4.2). Dropping it silently
        // would leave an agent blocked on a prompt this window had taken
        // off the screen.
        if self.confirm.is_some() {
            self.decide(Decision::Deny, window, cx);
        }
        // Bring the operator to it: a guarded allow is waiting on a click,
        // and the card it is about is on the Deck screen.
        cx.activate(true);
        window.activate_window();
        self.screen = Screen::Deck;
        if let Some(agent) = request.agent.clone() {
            self.deck.selected = Some(agent);
        }
        let card = self.card_of(request.agent.as_ref());
        let summary = confirm::summary(&request, card.as_ref());
        let text = card
            .as_ref()
            .map(|card| present::card_text(card).to_string())
            .filter(|text| !text.is_empty());
        self.confirm = Some(Confirming {
            request,
            summary,
            text,
        });
        self.dialog_opened(window);
        cx.notify();
    }

    /// The dialog's answer. [`Decision::Deny`] is what a dismissal runs too —
    /// Escape and the scrim both land here, and neither can allow (§4.2).
    fn decide(&mut self, decision: Decision, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Confirming { request, .. }) = self.confirm.take() else {
            return;
        };
        let card = self.card_of(request.agent.as_ref());
        match confirm::action(&request, card.as_ref(), decision) {
            Some(action) => self.act(action, window, cx),
            // The dialog's words were fixed when it was raised, but the
            // decision is checked against the mirror as it stands now — so a
            // card that was answered from the deck (or cleared) while the
            // dialog was up has nothing left to allow. Saying so is the
            // difference between "the click did nothing" and "the click was
            // ignored".
            None if decision == Decision::Allow => {
                self.toast("that request is no longer open", window, cx);
            }
            None => {}
        }
        cx.notify();
    }

    pub(crate) fn card_of(&self, agent: Option<&AgentId>) -> Option<Card> {
        self.state
            .mirror()
            .agents
            .get(agent?)?
            .card
            .as_ref()
            .cloned()
    }

    /// The confirmation dialog. Allow is offered only while the live card
    /// still has an allow **and** this build is the trusted principal; every
    /// other way out denies. There is no third door.
    fn render_confirm(&self, confirming: &Confirming, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let live = self.card_of(confirming.request.agent.as_ref());
        let allow =
            confirm::allow_offered(&confirming.request, live.as_ref(), self.state.trusted());
        v_flex()
            .p(px(20.))
            .child(
                h_flex()
                    .items_start()
                    .gap(px(14.))
                    .child(
                        div()
                            .flex_none()
                            .size(px(36.))
                            .rounded(px(10.))
                            .bg(p.warning.alpha(0.12))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(ui::icon(ic::DANGER, 18., p.warning_muted)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(6.))
                            .child(
                                div()
                                    .ts(15., 20.)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Confirm this operation"),
                            )
                            .child(
                                div()
                                    .ts(13., 19.)
                                    .text_color(p.muted)
                                    .child(confirming.summary.clone()),
                            ),
                    ),
            )
            .children(confirming.text.clone().map(|text| {
                div()
                    .id("confirm-text")
                    .mt(px(16.))
                    .max_h(px(180.))
                    .overflow_y_scroll()
                    .rounded(px(10.))
                    .bg(p.wash(0.05))
                    .px(px(12.))
                    .py(px(10.))
                    .ts(12.5, 19.)
                    .mono()
                    .child(text)
            }))
            .child(div().mt(px(14.)).ts(12., 17.).text_color(p.faint).child(if allow {
                "Allowing re-issues the call as this app, which is the trusted principal. \
                 Closing this dialog denies it."
            } else {
                "This request can only be denied here: there is no allow to re-issue, or this \
                 build was not verified as the trusted principal. Closing this dialog denies it."
            }))
            .child(
                h_flex()
                    .mt(px(20.))
                    .gap(px(8.))
                    .justify_end()
                    .child(Btn::new("confirm-deny", "Deny").on_click(cx.listener(
                        |this, _, window, cx| this.decide(Decision::Deny, window, cx),
                    )))
                    .when(allow, |this| {
                        this.child(Btn::new("confirm-allow", "Allow").danger().on_click(
                            cx.listener(|this, _, window, cx| {
                                this.decide(Decision::Allow, window, cx);
                            }),
                        ))
                    }),
            )
    }

    /// The layer a dialog sits on: the scrim, the card, and Escape.
    fn render_dialog_layer(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let dialog = self.dialog()?;
        let p = *palette(cx);
        let (body, width) = match dialog {
            Dialog::Confirm => (self.render_confirm(self.confirm.as_ref()?, cx), 420.),
            Dialog::Token => (self.render_token_dialog(cx)?, 460.),
            Dialog::Plan => (self.render_plan_dialog(cx)?, 580.),
            Dialog::Recognition => (self.render_recognition_dialog(cx), 470.),
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
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.dismiss_dialog(window, cx);
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
                    v_flex()
                        .id("dialog")
                        .occlude()
                        .w(px(width))
                        .max_h(relative(0.88))
                        .rounded(px(16.))
                        .bg(p.dialog)
                        .border_1()
                        .border_color(p.hairline(0.10))
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

    // ---- the sidebar, the titlebar and the strips -----------------------

    /// §8.2's sidebar navigation. Each screen is fetched on arrival rather
    /// than polled: an Integrations screen that re-ran `integrations.status`
    /// every second would run `claude --version` and Codex's `hooks/list`
    /// every second with it.
    fn render_sidebar(&self, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let waiting = self
            .state
            .mirror()
            .agents
            .values()
            .filter(|a| a.state == AgentState::Question)
            .count();
        let hijacked = deck_gui_core::admin::Hijack::of(&self.state.mirror().service).is_some();
        let degraded = self.state.mirror().service.degraded.is_some();
        let row = |screen: Screen, suffix: Option<AnyElement>| -> Stateful<Div> {
            let here = screen == self.screen;
            h_flex()
                .id(screen.id())
                .h(px(32.))
                .px(px(10.))
                .gap(px(10.))
                .rounded(px(8.))
                .ts(13., 17.)
                .cursor_pointer()
                .map(|this| {
                    if here {
                        this.bg(p.selected).text_color(p.text).medium()
                    } else {
                        this.text_color(p.muted)
                            .hover(|s| s.bg(p.hover).text_color(p.text))
                    }
                })
                .child(ui::icon(
                    screen.icon(),
                    16.,
                    if here { p.text } else { p.muted },
                ))
                .child(div().flex_1().min_w_0().child(screen.label()))
                .children(suffix)
                .on_click(cx.listener(move |this, _, window, cx| this.go(screen, window, cx)))
        };
        let count = (waiting > 0)
            .then(|| ui::badge(waiting.to_string(), ui::Tint::Danger, &p).into_any_element());
        let warn = |on: bool| on.then(|| ui::dot(p.warning, 6.).into_any_element());
        v_flex()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_none()
            .bg(p.wash(0.05))
            .border_r_1()
            .border_color(p.border)
            .child(self.titlebar_band("sidebar-titlebar"))
            .child(
                h_flex().px(px(16.)).py(px(14.)).gap(px(10.))
                    .child(ui::brand_mark(32.))
                    .child(v_flex().ts(12., 16.).medium()
                        .child("OpenWalkie")
                        .child("Talkie")),
            )
            .child(
                v_flex()
                    .px(px(8.))
                    .pt(px(6.))
                    .gap(px(2.))
                    .child(row(Screen::Deck, count))
                    .child(row(Screen::Usage, None))
                    .child(row(Screen::Integrations, warn(hijacked)))
                    .child(row(Screen::Voice, None))
                    .child(row(Screen::Service, warn(degraded && !hijacked)))
                    .child(div().h(px(14.)))
                    .child(row(Screen::Welcome, None)),
            )
            .child(div().flex_1())
            .child(self.render_status(&p))
    }

    /// The foot of the sidebar: whether the service is there, and the one
    /// standing fact about this build (§4.2's untrusted badge).
    fn render_status(&self, p: &Palette) -> Div {
        let connected = self.state.connected();
        let service = &self.state.mirror().service;
        let agents = self.state.mirror().agents.len();
        let (dot, title, meta) = if !connected {
            (
                p.faint,
                "Service not running".to_string(),
                "Showing the last state it sent".to_string(),
            )
        } else if service.degraded.is_some() {
            (
                p.warning,
                "Service degraded".to_string(),
                service.degraded.clone().unwrap_or_default(),
            )
        } else {
            let version = if service.version.is_empty() {
                "deckd".to_string()
            } else {
                format!("deckd {}", service.version)
            };
            let agents = match agents {
                1 => "1 agent".to_string(),
                n => format!("{n} agents"),
            };
            (
                p.success,
                "Service running".to_string(),
                format!("{version} \u{b7} {agents}"),
            )
        };
        v_flex()
            .px(px(16.))
            .pt(px(12.))
            .pb(px(14.))
            .gap(px(10.))
            .border_t_1()
            .border_color(p.divider())
            .child(
                h_flex()
                    .items_start()
                    .gap(px(10.))
                    .child(div().pt(px(5.)).child(ui::dot(dot, 7.)))
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(div().ts(12.5, 17.).medium().text_color(p.text).child(title))
                            .child(div().ts(11.5, 15.).text_color(p.muted).truncate().child(meta)),
                    ),
            )
            .when(connected && !self.state.trusted(), |this| {
                // §4.2: an untrusted GUI still observes and still operates.
                // It just cannot allow a permission or inject without going
                // through the confirmation path, so this is a note and not
                // an error.
                this.child(
                    h_flex()
                        .items_start()
                        .gap(px(8.))
                        .child(div().pt(px(1.)).child(ui::icon(ic::INFO, 13., p.warning_muted)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .ts(11.5, 15.)
                                .text_color(p.warning_muted)
                                .child("Not verified as the trusted GUI: this build cannot allow permissions."),
                        ),
                )
            })
    }

    /// The transparent strip the traffic lights sit in. Double-clicking it
    /// does what double-clicking a title bar does on this Mac.
    fn titlebar_band(&self, id: &'static str) -> Stateful<Div> {
        div()
            .id(id)
            .h(px(TITLEBAR_HEIGHT))
            .w_full()
            .flex_none()
            .on_click(|event, window, _| {
                if event.click_count() == 2 {
                    window.titlebar_double_click();
                }
            })
    }

    /// Open a screen, and ask for what it draws if it has nothing yet.
    pub(crate) fn go(&mut self, screen: Screen, window: &mut Window, cx: &mut Context<Self>) {
        if screen != self.screen {
            self.page_scroll.set_offset(gpui::point(px(0.), px(0.)));
        }
        self.screen = screen;
        match screen {
            Screen::Deck | Screen::Usage => {}
            Screen::Integrations => self.refresh_integrations(false, window, cx),
            Screen::Voice => self.refresh_voice(false, window, cx),
            Screen::Service => self.refresh_service(false, window, cx),
            Screen::Welcome => self.refresh_welcome(false, window, cx),
        }
        cx.notify();
    }

    /// The strip over every screen when the service is not there (§8.2's
    /// "Service not running"), and the quieter one while the mirror is
    /// being re-read. Neither is an error state for the window: the mirror
    /// underneath stays on screen, greyed.
    fn render_strips(&self, cx: &Context<Self>) -> Option<Div> {
        let p = *palette(cx);
        let connected = self.state.connected();
        if !connected {
            // The Service screen has Start in its own header.
            let start = (self.screen != Screen::Service).then(|| {
                Btn::new("start-service", "Start")
                    .solid()
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.start_service(window, cx);
                    }))
                    .into_any_element()
            });
            return Some(
                div().px(px(24.)).pb(px(10.)).child(ui::strip(
                    Severity::Warning,
                    Some("Service not running".into()),
                    Some(
                        div()
                            .child(
                                "What you see is the last state it sent. Start it and this \
                                 window reconnects on its own.",
                            )
                            .into_any_element(),
                    ),
                    start
                        .into_iter()
                        .chain(std::iter::once(
                            Btn::new("open-log", "Open log")
                            .quiet()
                            .small()
                            .tooltip("Opens the launchd log, which says why a service will not start")
                                .on_click(|_, _, cx: &mut App| {
                                    cx.open_with_system(&app::launchd_log());
                                })
                                .into_any_element(),
                        ))
                        .collect(),
                    &p,
                )),
            );
        }
        self.state.resyncing().then(|| {
            div().px(px(24.)).pb(px(10.)).child(
                h_flex()
                    .w_full()
                    .gap(px(8.))
                    .px(px(12.))
                    .h(px(30.))
                    .rounded(px(8.))
                    .bg(p.wash(0.05))
                    .ts(12., 16.)
                    .text_color(p.muted)
                    .child(ui::spinner(13., p.muted))
                    .child("Re-reading the service's state\u{2026}"),
            )
        })
    }

    pub(crate) fn start_service(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toast("Starting the service\u{2026}", window, cx);
        // The one piece of I/O this crate does itself, and it goes where all
        // I/O goes: the bridge's runtime, never the UI thread.
        let done = app::start_service(&cx.global::<app::Tokio>().0);
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Err(why)) = done.recv().await {
                this.update_in(cx, |this, window, cx| {
                    this.toast(format!("Could not start the service: {why}"), window, cx);
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
        // The sidebar and its 1 px rule come off the window's width.
        self.content_width = f32::from(window.viewport_size().width) - SIDEBAR_WIDTH - 1.;
        // Disconnected: the last mirror is greyed, not blanked (§8.2). A
        // roster from two seconds ago is worth more to the operator than an
        // empty window — but every button on it would fail now, so they are
        // off and the pane is dimmed to say why.
        let dim = !self.state.connected();
        // Each pane is its own element type (the scrolling ones are
        // `Stateful<Div>`), so they meet here as `AnyElement`.
        let pane = match self.screen {
            Screen::Deck => self.render_deck(dim, cx).into_any_element(),
            Screen::Usage => self.render_usage(dim, cx).into_any_element(),
            Screen::Integrations => self.render_integrations(dim, cx).into_any_element(),
            Screen::Voice => self.render_voice(dim, cx).into_any_element(),
            Screen::Service => self.render_service(dim, cx).into_any_element(),
            Screen::Welcome => self.render_welcome(dim, cx).into_any_element(),
        };
        div()
            .size_full()
            .relative()
            .font_family(SANS)
            .ts(14., 20.)
            .text_color(p.text)
            .child(
                h_flex().size_full().child(self.render_sidebar(cx)).child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .child(self.titlebar_band("titlebar"))
                        .children(self.render_strips(cx))
                        .child(div().flex_1().min_h_0().child(pane)),
                ),
            )
            .children(self.render_dialog_layer(cx))
            // `Root` owns the notifications but does not draw them: the
            // view that raises them renders their layer.
            .children(Root::render_notification_layer(window, cx))
    }
}

/// What a failed call is called in a toast.
fn action_name(action: &UiAction) -> &'static str {
    match action {
        UiAction::ImageGet { .. } => "Load image",
        UiAction::Answer { .. } => "Answer",
        UiAction::Snooze { .. } => "Snooze",
        UiAction::Ack { .. } => "Acknowledge",
        UiAction::Focus { .. } => "Focus",
        UiAction::Order { .. } => "Order",
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
    }
}

/// `{delivered:false, reason?}` from an `op.answer`/`op.order` that went
/// through and could not be delivered (§4.7). `None` when it was delivered,
/// or when the reply says nothing about delivery at all.
fn undelivered(result: &Value) -> Option<String> {
    if result.get("delivered")?.as_bool() != Some(false) {
        return None;
    }
    let reason = result
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("not delivered");
    Some(reason.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use deck_model::CardId;

    #[test]
    fn every_ui_action_has_a_name_a_toast_can_start_a_sentence_with() {
        let agent = AgentId("ag_1".into());
        let card = CardId(17);
        for action in [
            UiAction::Answer {
                agent: agent.clone(),
                card,
                choice: Some(0),
                text: None,
            },
            UiAction::Snooze {
                agent: agent.clone(),
                card,
            },
            UiAction::Ack {
                agent: agent.clone(),
                card,
            },
            UiAction::Focus {
                agent: agent.clone(),
            },
            UiAction::Order {
                agent,
                text: "go".into(),
            },
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
        ] {
            let name = action_name(&action);
            assert!(!name.is_empty());
            assert!(
                name.chars().next().is_some_and(char::is_uppercase),
                "{name} starts a sentence in a toast"
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
    fn a_delivery_that_did_not_happen_is_read_off_the_reply_and_nothing_else_is() {
        use serde_json::json;
        assert_eq!(
            undelivered(&json!({"delivered": true, "via": "hold"})),
            None
        );
        assert_eq!(
            undelivered(&json!({"delivered": false, "reason": "timeout"})).as_deref(),
            Some("timeout")
        );
        assert_eq!(
            undelivered(&json!({"delivered": false})).as_deref(),
            Some("not delivered")
        );
        // `op.focus` and `state.get` say nothing about delivery.
        assert_eq!(undelivered(&json!({"ok": true})), None);
        assert_eq!(undelivered(&json!({"rev": 12})), None);
        assert_eq!(undelivered(&Value::Null), None);
    }

    #[test]
    fn the_window_opens_on_the_deck_unless_a_gate_asks_for_another_screen() {
        assert_eq!(Screen::parse("deck"), Screen::Deck);
        assert_eq!(Screen::parse("Integrations"), Screen::Integrations);
        assert_eq!(Screen::parse(" service "), Screen::Service);
        assert_eq!(Screen::parse("voice"), Screen::Voice);
        assert_eq!(Screen::parse("Welcome"), Screen::Welcome);
        // Anything else is the Deck, which is where the window opens anyway
        // — a typo in an environment variable must not leave a blank window.
        assert_eq!(Screen::parse("settings"), Screen::Deck);
        assert_eq!(Screen::parse(""), Screen::Deck);
        assert_eq!(SCREEN_ENV, "OPENWALKIETALKIE_SCREEN");
    }

    #[test]
    fn the_sidebar_names_the_five_screens_v1_ships_and_gives_each_its_own_id() {
        let labels: Vec<&str> = Screen::ALL.iter().map(|s| s.label()).collect();
        assert_eq!(
            labels,
            vec!["Deck", "Usage", "Integrations", "Voice", "Service", "Welcome"]
        );
        // §8.2 asks for a Welcome that is re-openable, and this is what
        // re-opens it: a row, like every other screen.
        assert!(Screen::ALL.contains(&Screen::Welcome));
        let ids: Vec<&str> = Screen::ALL.iter().map(|s| s.id()).collect();
        for (i, id) in ids.iter().enumerate() {
            assert!(id.starts_with("nav-"));
            assert!(!ids[i + 1..].contains(id), "{id} is used twice");
        }
    }

    #[test]
    fn the_repaint_tick_is_the_granularity_the_countdowns_are_read_to() {
        assert_eq!(TICK, Duration::from_secs(1));
        assert!(RESYNC_RETRY > TICK, "a retry storm is worse than a wait");
    }
}
