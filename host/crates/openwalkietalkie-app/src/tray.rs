//! The status item (§8.2's tray row): the aggregate dot in the menu bar and
//! the six things that can be done from it without opening the window.
//!
//! **What the tray says is not decided here.** `deck_gui_core::tray::Summary`
//! computes the dot, the tooltip and both ticks from the same mirror the
//! window draws, and this module is the AppKit end of it: build the menu
//! once, and redraw it whenever the `Shell` hands over a new summary. That
//! split is why the tray and the window cannot disagree about how many
//! questions are waiting — there is one answer and two places that draw it.
//!
//! A click goes the long way round on purpose. `muda` calls its handler from
//! the menu's own thread, so the handler does exactly one thing —
//! `events::post(AppEvent::Tray(..))` — and the `Shell`'s bus pump turns it
//! into the same [`UiAction`](deck_gui_core::UiAction) a button on a screen
//! sends. Nothing in the menu bar touches the mirror, and no tick is ever set
//! by a click: it is set by the event the daemon sends back.
//!
//! The pin is measured (contract §1 M1): `tray-icon` **0.25.1**, not the
//! design's 0.19, because 0.19 wants `objc2 ^0.5.2` against this workspace's
//! `objc2 =0.6.4` and cargo answers that by building both — five objc2
//! crates at two major versions each, for a menu.
use crate::present;
use deck_gui_core::i18n::Language;
use deck_gui_core::tray::{Summary, TrayCommand, quit_tooltip};
use deck_gui_core::{events, AppEvent};
use gpui::Global;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// The status item and the two items whose tick has to be kept in step with
/// the mirror, parked in a `gpui` [`Global`] so they live exactly as long as
/// the app.
///
/// It is a global and not a field of the `Shell` for one reason: dropping a
/// `TrayIcon` removes it from the menu bar, and a window that can be closed
/// (this one hides on close, §8.1) must not be able to take the tray with
/// it. Tray-only mode is the whole point of the item.
pub struct Tray {
    icon: TrayIcon,
    mute: CheckMenuItem,
    pause_hooks: CheckMenuItem,
    labels: Vec<(TrayCommand, MenuItem)>,
    language: Language,
    /// The last dot drawn, so a redraw that would change nothing does not
    /// rebuild a PNG on every event the bus delivers.
    dot: Option<deck_model::Tone>,
}

impl Global for Tray {}

/// Build the status item and install the click handler.
///
/// `None` when the menu bar refused it, which is not fatal and must not be:
/// a headless session, a locked screen at login or a Mac with no status bar
/// leaves the window working exactly as it did. The caller says so and
/// carries on.
pub fn install() -> Option<Tray> {
    install_localized(Language::English)
}

pub fn install_localized(language: Language) -> Option<Tray> {
    let menu = Menu::new();
    let open = MenuItem::with_id(
        TrayCommand::Open.id(),
        TrayCommand::Open.label_localized(language),
        true,
        None,
    );
    let mute = CheckMenuItem::with_id(
        TrayCommand::Mute.id(),
        TrayCommand::Mute.label_localized(language),
        true,
        false,
        None,
    );
    // Disabled until `config.get` has answered: a tick that guessed would
    // re-enable hooks the operator had paused in `config.toml` by hand.
    let pause_hooks = CheckMenuItem::with_id(
        TrayCommand::PauseHooks.id(),
        TrayCommand::PauseHooks.label_localized(language),
        false,
        false,
        None,
    );
    let updates = MenuItem::with_id(
        TrayCommand::CheckUpdates.id(),
        TrayCommand::CheckUpdates.label_localized(language),
        true,
        None,
    );
    let stop = MenuItem::with_id(
        TrayCommand::StopService.id(),
        TrayCommand::StopService.label_localized(language),
        true,
        None,
    );
    let quit = MenuItem::with_id(
        TrayCommand::Quit.id(),
        TrayCommand::Quit.label_localized(language),
        true,
        None,
    );
    let built = menu
        .append(&open)
        .and_then(|()| menu.append(&PredefinedMenuItem::separator()))
        .and_then(|()| menu.append(&mute))
        .and_then(|()| menu.append(&pause_hooks))
        .and_then(|()| menu.append(&PredefinedMenuItem::separator()))
        .and_then(|()| menu.append(&updates))
        .and_then(|()| menu.append(&stop))
        .and_then(|()| menu.append(&quit));
    if let Err(e) = built {
        eprintln!("openwalkietalkie-app: could not build the tray menu: {e}");
        return None;
    }
    let icon = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(quit_tooltip(language))
        .with_icon(icon_for(deck_model::Tone::Gray)?)
        // AppKit tints the alpha mask for the menu bar and its selected state.
        .with_icon_as_template(true)
        .build()
        .map_err(|e| eprintln!("openwalkietalkie-app: no status item: {e}"))
        .ok()?;
    // The menu's own thread calls this. `events::post` is a `try_send` on an
    // unbounded channel, so it needs no executor and blocks nobody — which
    // is exactly why the bus is an `async_channel` and not an mpsc.
    MenuEvent::set_event_handler(Some(|event: MenuEvent| {
        if let Some(command) = TrayCommand::parse(event.id.as_ref()) {
            events::post(AppEvent::Tray(command));
        }
    }));
    Some(Tray {
        icon,
        mute,
        pause_hooks,
        labels: vec![(TrayCommand::Open, open), (TrayCommand::CheckUpdates, updates), (TrayCommand::StopService, stop), (TrayCommand::Quit, quit)],
        language,
        dot: Some(deck_model::Tone::Gray),
    })
}

impl Tray {
    /// Redraw the status item from the mirror.
    ///
    /// Called on every bus event, so the two things that cost something —
    /// rasterising the icon and encoding it as a PNG — are skipped whenever
    /// the dot has not moved.
    pub fn update(&mut self, summary: &Summary) {
        self.update_localized(summary, Language::English);
    }

    pub fn update_localized(&mut self, summary: &Summary, language: Language) {
        if self.language != language {
            for (command, item) in &self.labels {
                item.set_text(command.label_localized(language));
            }
            self.mute.set_text(TrayCommand::Mute.label_localized(language));
            self.pause_hooks.set_text(TrayCommand::PauseHooks.label_localized(language));
            self.language = language;
        }
        if self.dot != Some(summary.dot) {
            if let Some(icon) = icon_for(summary.dot) {
                if self.icon.set_icon_with_as_template(Some(icon), true).is_ok() {
                    self.dot = Some(summary.dot);
                }
            }
        }
        let _ = self.icon.set_tooltip(Some(summary.tooltip()));
        // Both ticks show what the **daemon** last reported. A click sets
        // neither: the event that comes back does.
        self.mute.set_checked(summary.muted);
        match summary.hooks_enabled {
            Some(enabled) => {
                self.pause_hooks.set_enabled(true);
                self.pause_hooks.set_checked(!enabled);
            }
            None => {
                self.pause_hooks.set_enabled(false);
                self.pause_hooks.set_checked(false);
            }
        }
    }
}

/// The dot as an icon `tray-icon` will take. `None` only when the bitmap is
/// the wrong length for its dimensions, which is a bug in
/// [`present::tray_icon_rgba`] and not a runtime condition.
fn icon_for(dot: deck_model::Tone) -> Option<Icon> {
    Icon::from_rgba(
        present::tray_icon_rgba(dot),
        present::TRAY_ICON_PX,
        present::TRAY_ICON_PX,
    )
    .map_err(|e| eprintln!("openwalkietalkie-app: bad tray icon: {e}"))
    .ok()
}
