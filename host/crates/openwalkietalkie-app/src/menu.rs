//! The status item and its menu, and the MenuBar board drawn in the
//! window.
//!
//! **What the menu says is not decided here.** `deck_gui_core`'s
//! [`MenuModel`] reads the mirror, the pause as the Integrations page read
//! it, the [`attention`] list and, with no service answering, why not as the
//! Deck page tells it (`Shell::service_absence`), and [`lines`] lays the
//! model out as the menu's lines, in order. This module is the AppKit end of those lines —
//! the status item, built once by [`install`] and kept in step by
//! [`Shell::update_menu`] — and draws the same lines in the window
//! ([`Shell::render_menu_preview`]), so the board the harness captures is
//! the menu the menu bar shows.
//!
//! A click goes the long way round on purpose. `muda` calls its handler
//! from the menu's own thread, so the handler only posts the item's
//! [`MenuCommand`] to the bus, and the shell's pump hands it to
//! [`Shell::on_menu`], which sends what the model names: the one path to
//! the service a control on a page takes. No tick is set by a click; the
//! event the service sends back sets it.
//!
//! The icon is drawn here, in code: the sidebar's deck glyph as a 36 px
//! template image, its three states told apart by shape — a slash across
//! the screen when the deck is not linked, a dot at the top right when it
//! needs care ([`icon_rgba`]). AppKit tints it for the menu bar.
use crate::present;
use crate::shell::{Screen, Shell};
use crate::style::{palette, Palette};
use crate::ui::{self, ic, Type as _};
use deck_gui_core::attention::{attention, Extras};
use deck_gui_core::i18n::Language;
use deck_gui_core::{events, AppEvent, IconState, MenuCommand, MenuModel, NoService};
use deck_model::Snapshot;
use gpui::{
    div, hsla, img, point, prelude::*, px, AnyElement, BoxShadow, Context, Div, Global, Hsla,
    Image, ImageFormat, Rgba, Subscription, Window,
};
use gpui_component::{h_flex, v_flex};
use std::sync::Arc;
use tray_icon::menu::{CheckMenuItem, Menu as Items, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// The status item's bitmap, in pixels. AppKit shows it 18 pt high, so 36
/// is exactly 2x on a Retina menu bar and needs no resampling.
pub const ICON_PX: u32 = 36;

/// The status item's size in points, as AppKit draws it and as the board
/// draws it in the window.
const ICON_PT: f32 = 18.;

/// Pixels per unit of the deck glyph's 24-unit grid.
const GRID: f32 = ICON_PX as f32 / 24.;

/// The glyph's stroke, in units of its grid.
const STROKE: f32 = 1.5;

/// The id of the menu's first line. The line is disabled, so it is never
/// clicked, and no [`MenuCommand`] parses the id.
const STATUS: &str = "menu-status";

/// The MenuBar board, at the window's top-left.
const BOARD: (f32, f32) = (420., 320.);

/// The family a native menu is drawn in, which the board's menu is too.
const SYSTEM_FONT: &str = ".SystemUIFont";

/// The most a stored deflate block holds.
const STORED_MAX: usize = u16::MAX as usize;

/// One line of the status item's menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    /// The first line: how the deck is linked and its charge, or why it
    /// is not. Disabled: it says, and does nothing.
    Status(String),
    /// An item that does something: its label, whether it can be pressed
    /// now, and its tick when it is a toggle (`Some(true)` is ticked).
    Item {
        command: MenuCommand,
        label: &'static str,
        enabled: bool,
        checked: Option<bool>,
    },
    Separator,
}

/// The menu for `model`, line by line, in `language`: the status line,
/// **Show Link Problem…** while the link has one, the two toggles, then
/// the window's own three items.
///
/// `connected` is the socket. The two toggles change the service, so they
/// are disabled while it is not there, with the ticks it last reported;
/// **Pause Integrations** also waits for the pause to have been read, and
/// a menu that guessed would resume what the owner had paused.
pub fn lines(model: &MenuModel, connected: bool, language: Language) -> Vec<Line> {
    let item = |command: MenuCommand, enabled: bool, checked: Option<bool>| Line::Item {
        command,
        label: command.label_localized(language),
        enabled,
        checked,
    };
    let mut lines = vec![Line::Status(model.status.clone())];
    if model.link_problem {
        lines.push(item(MenuCommand::ShowLinkProblem, true, None));
    }
    lines.extend([
        Line::Separator,
        item(MenuCommand::AutoAnnounce, connected, Some(model.announce)),
        item(
            MenuCommand::PauseIntegrations,
            connected && model.paused.is_some(),
            Some(model.paused == Some(true)),
        ),
        Line::Separator,
        item(MenuCommand::Open, true, None),
        item(MenuCommand::CheckUpdates, true, None),
        Line::Separator,
        item(MenuCommand::Quit, true, None),
    ]);
    lines
}

/// What the status item is, in words: its tooltip, which says what the
/// icon's shape means.
pub fn describe(state: IconState, language: Language) -> &'static str {
    match state {
        IconState::Linked => language.text("OpenWalkieTalkie status item, the deck is linked"),
        IconState::NotLinked => {
            language.text("OpenWalkieTalkie status item, the deck is not linked")
        }
        IconState::Care => language.text("OpenWalkieTalkie status item, the deck needs care"),
    }
}

/// The status item and the items whose words and ticks follow the mirror,
/// parked in a `gpui` [`Global`] so they live exactly as long as the app:
/// dropping the icon would take it out of the menu bar, and a window that
/// hides on close must not be able to.
pub struct Menu {
    icon: TrayIcon,
    /// A handle on the menu the icon shows: **Show Link Problem…** is put
    /// in and taken out of it.
    items: Items,
    status: MenuItem,
    link_problem: MenuItem,
    announce: CheckMenuItem,
    pause: CheckMenuItem,
    open: MenuItem,
    updates: MenuItem,
    quit: MenuItem,
    // What the menu bar shows now, so an update that would change nothing
    // touches nothing: the icon, the language of its tooltip, the lines,
    // and whether **Show Link Problem…** is in the menu.
    shown_icon: IconState,
    shown_tooltip: (IconState, Language),
    shown_lines: Vec<Line>,
    problem_in: bool,
    /// The window's redraws, followed from the first update on
    /// ([`Shell::update_menu`]).
    watch: Option<Subscription>,
}

impl Global for Menu {}

/// Build the status item and install the click handler.
///
/// `None` when the menu bar refused it, which is not fatal: the window
/// works exactly as it did, and the caller carries on. Windows has one too,
/// in the notification area (tray-icon's native backend, driven by the
/// message loop gpui already runs). Elsewhere there is none: the tray
/// backend there is GTK, which this app never starts, and a status item is
/// the desktop bar's job.
pub fn install(language: Language) -> Option<Menu> {
    if cfg!(not(any(target_os = "macos", windows))) {
        return None;
    }
    // Nothing is known yet: no service has answered. The first event the
    // bus delivers puts the mirror's words in, and why none answers.
    let model = MenuModel::of(
        &Snapshot::default(),
        false,
        NoService::default(),
        None,
        &[],
        language,
    );
    let label = |command: MenuCommand| command.label_localized(language);
    let status = MenuItem::with_id(STATUS, &model.status, false, None);
    let link_problem = MenuItem::with_id(
        MenuCommand::ShowLinkProblem.id(),
        label(MenuCommand::ShowLinkProblem),
        true,
        None,
    );
    let announce = CheckMenuItem::with_id(
        MenuCommand::AutoAnnounce.id(),
        label(MenuCommand::AutoAnnounce),
        false,
        false,
        None,
    );
    let pause = CheckMenuItem::with_id(
        MenuCommand::PauseIntegrations.id(),
        label(MenuCommand::PauseIntegrations),
        false,
        false,
        None,
    );
    let open = MenuItem::with_id(MenuCommand::Open.id(), label(MenuCommand::Open), true, None);
    let updates = MenuItem::with_id(
        MenuCommand::CheckUpdates.id(),
        label(MenuCommand::CheckUpdates),
        true,
        None,
    );
    let quit = MenuItem::with_id(MenuCommand::Quit.id(), label(MenuCommand::Quit), true, None);
    let items = Items::new();
    let built = items.append_items(&[
        &status,
        &PredefinedMenuItem::separator(),
        &announce,
        &pause,
        &PredefinedMenuItem::separator(),
        &open,
        &updates,
        &PredefinedMenuItem::separator(),
        &quit,
    ]);
    if let Err(e) = built {
        eprintln!("openwalkietalkie-app: could not build the status item's menu: {e}");
        return None;
    }
    let icon = TrayIconBuilder::new()
        .with_menu(Box::new(items.clone()))
        .with_icon(icon_for(model.icon)?)
        // AppKit tints the alpha mask for the menu bar and its selected
        // state.
        .with_icon_as_template(true)
        .with_tooltip(describe(model.icon, language))
        .build()
        .map_err(|e| eprintln!("openwalkietalkie-app: no status item: {e}"))
        .ok()?;
    // The menu's own thread calls this. `events::post` is a `try_send` on
    // an unbounded channel, so it needs no executor and blocks nobody.
    MenuEvent::set_event_handler(Some(|event: MenuEvent| {
        if let Some(command) = MenuCommand::parse(event.id.as_ref()) {
            events::post(AppEvent::Menu(command));
        }
    }));
    let mut menu = Menu {
        icon,
        items,
        status,
        link_problem,
        announce,
        pause,
        open,
        updates,
        quit,
        shown_icon: model.icon,
        shown_tooltip: (model.icon, language),
        shown_lines: Vec::new(),
        problem_in: false,
        watch: None,
    };
    menu.show(model.icon, lines(&model, false, language), language);
    Some(menu)
}

impl Menu {
    /// Put `lines` and the icon for `icon` in the menu bar, touching only
    /// what changed. A refusal from AppKit is said once and not retried:
    /// it is a bug here, not something the next second would mend.
    fn show(&mut self, icon: IconState, lines: Vec<Line>, language: Language) {
        if self.shown_icon != icon {
            self.shown_icon = icon;
            if let Some(image) = icon_for(icon) {
                if let Err(e) = self.icon.set_icon_with_as_template(Some(image), true) {
                    eprintln!("openwalkietalkie-app: the status item kept its icon: {e}");
                }
            }
        }
        if self.shown_tooltip != (icon, language) {
            self.shown_tooltip = (icon, language);
            if let Err(e) = self.icon.set_tooltip(Some(describe(icon, language))) {
                eprintln!("openwalkietalkie-app: the status item kept its tooltip: {e}");
            }
        }
        if self.shown_lines != lines {
            self.apply(&lines);
            self.shown_lines = lines;
        }
    }

    /// Each line's words, availability and tick on its item, and **Show
    /// Link Problem…** in the menu exactly when a line names it.
    fn apply(&mut self, lines: &[Line]) {
        let mut problem = false;
        for line in lines {
            match line {
                Line::Status(text) => self.status.set_text(text),
                Line::Separator => {}
                Line::Item {
                    command,
                    label,
                    enabled,
                    checked,
                } => {
                    problem |= *command == MenuCommand::ShowLinkProblem;
                    if let Some(item) = self.toggle(*command) {
                        item.set_text(label);
                        item.set_enabled(*enabled);
                        item.set_checked(checked.unwrap_or(false));
                    } else if let Some(item) = self.item(*command) {
                        item.set_text(label);
                        item.set_enabled(*enabled);
                    }
                }
            }
        }
        if problem != self.problem_in {
            // Right under the status line, the one place it ever goes.
            let moved = if problem {
                self.items.insert(&self.link_problem, 1)
            } else {
                self.items.remove(&self.link_problem)
            };
            match moved {
                Ok(()) => self.problem_in = problem,
                Err(e) => {
                    eprintln!("openwalkietalkie-app: could not change the status item's menu: {e}");
                }
            }
        }
    }

    /// The item with a tick for `command`.
    fn toggle(&self, command: MenuCommand) -> Option<&CheckMenuItem> {
        match command {
            MenuCommand::AutoAnnounce => Some(&self.announce),
            MenuCommand::PauseIntegrations => Some(&self.pause),
            _ => None,
        }
    }

    /// The item without one.
    fn item(&self, command: MenuCommand) -> Option<&MenuItem> {
        match command {
            MenuCommand::ShowLinkProblem => Some(&self.link_problem),
            MenuCommand::Open => Some(&self.open),
            MenuCommand::CheckUpdates => Some(&self.updates),
            MenuCommand::Quit => Some(&self.quit),
            MenuCommand::AutoAnnounce | MenuCommand::PauseIntegrations => None,
        }
    }
}

/// The icon for `state` as `tray-icon` takes it. `None` only when the
/// bitmap is the wrong length for its size, which is a bug in
/// [`icon_rgba`] and not a runtime condition.
fn icon_for(state: IconState) -> Option<Icon> {
    Icon::from_rgba(icon_rgba(state), ICON_PX, ICON_PX)
        .map_err(|e| eprintln!("openwalkietalkie-app: bad status item icon: {e}"))
        .ok()
}

impl Shell {
    /// Redraw the status item from the mirror. The shell calls this after
    /// every event the bus delivers; from the first call on it also follows
    /// every redraw of the window, because some of what the menu reads
    /// arrives by no event: the pause, the old helpers and the firmware are
    /// replies the pages keep, and the link's drops leave the last hour as
    /// the clock moves. The window redraws on each reply and once a second.
    pub(crate) fn update_menu(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if !cx.has_global::<Menu>() {
            return;
        }
        if cx.global::<Menu>().watch.is_none() {
            let watch = cx.observe_self(|this, cx| this.sync_menu(cx));
            cx.update_global::<Menu, ()>(|menu, _| menu.watch = Some(watch));
        }
        self.sync_menu(cx);
    }

    /// The status item as the mirror and the pages' answers leave it now.
    fn sync_menu(&self, cx: &mut Context<Self>) {
        if !cx.has_global::<Menu>() {
            return;
        }
        let language = self.language();
        let model = self.menu_model();
        let lines = lines(&model, self.state.connected(), language);
        cx.update_global::<Menu, ()>(|menu, _| menu.show(model.icon, lines, language));
    }

    /// A click on the menu. Every item that changes anything becomes the
    /// action the menu's model names; **Open** and **Quit** are the
    /// window's own, and Quit ends this app alone: the service is a launch
    /// agent and keeps the deck working.
    pub(crate) fn on_menu(
        &mut self,
        command: MenuCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            MenuCommand::ShowLinkProblem => {
                crate::app::show(cx);
                self.go(Screen::Deck, window, cx);
                self.open_link_details(window, cx);
            }
            MenuCommand::AutoAnnounce => {
                let action = self.menu_model().announce_action();
                self.act(action, window, cx);
            }
            // Unread, the item is disabled; a click that raced the read
            // changes nothing rather than resume what the owner paused.
            MenuCommand::PauseIntegrations => {
                if let Some(paused) = self.integrations.paused() {
                    self.set_paused(!paused, window, cx);
                }
            }
            MenuCommand::Open => {
                crate::app::show(cx);
                window.activate_window();
            }
            MenuCommand::CheckUpdates => self.check_updates(window, cx),
            MenuCommand::Quit => cx.quit(),
        }
        cx.notify();
    }

    /// The MenuBar board, drawn over the window's top-left 420×320 from
    /// the menu's model: the menu bar with the status item as it is now,
    /// the three icons with their names, and the menu's [`lines`]. The
    /// icons are the status item's own bitmaps and the lines the native
    /// menu's own, so what the harness captures is what the menu bar
    /// shows; only the menu's look is drawn here. A screen of the harness
    /// alone, it covers the sidebar too.
    pub(crate) fn render_menu_preview(&self, cx: &Context<Self>) -> AnyElement {
        let p = *palette(cx);
        let model = self.menu_model();
        // The board's bar is 28 high with its rule under that, not inside.
        let bar = h_flex()
            .flex_none()
            .h(px(28. + 1.))
            .px(px(14.))
            .justify_end()
            .bg(p.side)
            .border_b_1()
            .border_color(p.line)
            .child(icon_tile(model.icon, &p));
        let states = [
            (IconState::Linked, self.tr("Linked")),
            (IconState::NotLinked, self.tr("Not linked")),
            (IconState::Care, self.tr("Needs care")),
        ];
        let legend = v_flex()
            .flex_none()
            .gap(px(14.))
            .pt(px(6.))
            .children(states.map(|(state, name)| {
                v_flex()
                    .w(px(76.))
                    .items_center()
                    .gap(px(6.))
                    .child(icon_tile(state, &p))
                    .child(div().ts(12., 16.).text_color(p.text2).child(name))
            }));
        let menu = v_flex()
            .flex_none()
            .w(px(276.))
            .p(px(5.))
            .rounded(px(8.))
            .bg(p.surf)
            .border_1()
            .border_color(p.edge)
            // The board's `0 14px 36px`. A CSS blur radius is twice the
            // Gaussian's deviation, and gpui's `blur_radius` is the
            // deviation itself.
            .shadow(vec![BoxShadow {
                color: hsla(0., 0., 0., 0.45),
                offset: point(px(0.), px(14.)),
                blur_radius: px(36. / 2.),
                spread_radius: px(0.),
            }])
            .children(
                lines(&model, self.state.connected(), self.language())
                    .iter()
                    .map(|line| menu_line(line, &p)),
            );
        let board = v_flex()
            .w(px(BOARD.0))
            .h(px(BOARD.1))
            .gap(px(8.))
            .child(bar)
            .child(
                h_flex()
                    .items_start()
                    .justify_between()
                    .pl(px(22.))
                    .pr(px(10.))
                    .child(legend)
                    .child(menu),
            );
        // The whole window: the shell draws this scene with no sidebar, so
        // the board starts at the window's own top-left corner.
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .left_0()
            .right_0()
            .occlude()
            .bg(p.win)
            .text_color(p.text)
            .child(board)
            .into_any_element()
    }

    /// The menu as the mirror and the pages' answers leave it now, and,
    /// with no service answering, why not (as the frame's strip says it).
    fn menu_model(&self) -> MenuModel {
        let extras = Extras {
            stale_helpers: self.integrations.stale_helpers(),
            firmware: self.service.firmware_available(),
        };
        let mirror = self.state.mirror();
        let connected = self.state.connected();
        let list = attention(mirror, connected, present::now(), &extras);
        MenuModel::of(
            mirror,
            connected,
            self.service_absence(),
            self.integrations.paused(),
            &list,
            self.language(),
        )
    }
}

/// A status item's 30×22 plate as the board draws the menu bar's: the
/// icon for `state`, the status item's own bitmap tinted as AppKit tints
/// a template image.
fn icon_tile(state: IconState, p: &Palette) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .w(px(30.))
        .h(px(22.))
        .rounded(px(5.))
        .bg(p.well)
        .child(img(icon_image(state, p.text)).flex_none().size(px(ICON_PT)))
}

/// One of [`lines`] as the board draws it: 24 high, a column for the tick,
/// the words in the system's font, greyed when the item cannot be pressed.
fn menu_line(line: &Line, p: &Palette) -> AnyElement {
    let (text, enabled, ticked) = match line {
        Line::Separator => {
            return div()
                .h(px(1.))
                .mx(px(10.))
                .my(px(5.))
                .bg(p.line)
                .into_any_element();
        }
        Line::Status(text) => (text.as_str(), false, false),
        Line::Item {
            label,
            enabled,
            checked,
            ..
        } => (*label, *enabled, *checked == Some(true)),
    };
    let color = if enabled { p.text } else { p.text3 };
    h_flex()
        .h(px(24.))
        .pl(px(8.))
        .pr(px(10.))
        .gap(px(6.))
        .rounded(px(5.))
        .font_family(SYSTEM_FONT)
        .ts(13., 18.)
        .text_color(color)
        .child(
            div()
                .flex_none()
                .w(px(14.))
                .flex()
                .items_center()
                .children(ticked.then(|| ui::icon(ic::CHECK, 12., color))),
        )
        .child(div().flex_1().min_w_0().truncate().child(text.to_string()))
        .into_any_element()
}

/// The status item's icon for `state`, as straight RGBA [`ICON_PX`]
/// square: the deck glyph in black, its coverage as alpha. That is a
/// template image: AppKit draws the alpha alone, in the menu bar's own
/// colour.
///
/// The glyph is the sidebar's (`icons/deck.svg`: a 24-unit grid, a 1.5
/// stroke, round caps) — the slab, the screen and the split key — at 1.5
/// px a unit. Not linked adds a slash across the screen, corner to corner
/// (`deck-off.svg`); care adds a dot at the top right, over the slab's
/// corner (`deck-care.svg`). Each pixel is as opaque as the shapes cover
/// it, measured from its centre.
pub fn icon_rgba(state: IconState) -> Vec<u8> {
    let side = ICON_PX as usize;
    let ink = glyph_ink();
    let mut pixels = vec![0u8; side * side * 4];
    for (i, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = (i % side, i / side);
        let at = ((x as f32 + 0.5) / GRID, (y as f32 + 0.5) / GRID);
        // Straight (un-premultiplied) RGBA, which is what
        // `Icon::from_rgba` takes: the ink, and the coverage.
        pixel[..3].copy_from_slice(&[ink; 3]);
        pixel[3] = (glyph(state, at) * 255.).round() as u8;
    }
    pixels
}

/// The glyph's grey level. Black on macOS, where AppKit tints the template
/// for the menu bar. Windows does not tint a tray icon, so it is drawn for
/// the taskbar's theme: white on a dark taskbar (the default), black on a
/// light one (`SystemUsesLightTheme`).
fn glyph_ink() -> u8 {
    #[cfg(windows)]
    {
        let light = crate::run_key::dword(
            r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
            "SystemUsesLightTheme",
        )
        .is_some_and(|value| value != 0);
        if light { 0 } else { 255 }
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// How much of the glyph for `state` covers the pixel centred on `at`, a
/// point of the glyph's grid: 0 to 1.
fn glyph(state: IconState, at: (f32, f32)) -> f32 {
    let stroke = |distance: f32| coverage((STROKE / 2. - distance.abs()) * GRID);
    let deck = stroke(rounded_rect(at, (12., 12.), (6., 9.5), 3.))
        .max(stroke(rounded_rect(at, (12., 8.5), (3.5, 3.5), 1.5)))
        .max(stroke(segment(at, (8.75, 15.25), (11.25, 15.25))))
        .max(stroke(segment(at, (12.75, 15.25), (15.25, 15.25))));
    match state {
        IconState::Linked => deck,
        IconState::NotLinked => deck.max(stroke(segment(at, (8.5, 12.), (15.5, 5.)))),
        IconState::Care => deck.max(coverage((2.6 - distance(at, (19., 4.5))) * GRID)),
    }
}

/// The signed distance from `at` to the edge of the rectangle centred on
/// `centre`, `half` its half-size, with corners of `radius`: negative
/// inside.
fn rounded_rect(at: (f32, f32), centre: (f32, f32), half: (f32, f32), radius: f32) -> f32 {
    let qx = (at.0 - centre.0).abs() - (half.0 - radius);
    let qy = (at.1 - centre.1).abs() - (half.1 - radius);
    qx.max(0.).hypot(qy.max(0.)) + qx.max(qy).min(0.) - radius
}

/// The distance from `at` to the segment from `a` to `b`.
fn segment(at: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((at.0 - a.0) * dx + (at.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0., 1.);
    distance(at, (a.0 + t * dx, a.1 + t * dy))
}

fn distance(a: (f32, f32), b: (f32, f32)) -> f32 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

/// Anti-aliasing: a signed distance in pixels, positive inside, as a
/// coverage from 0 to 1.
fn coverage(distance: f32) -> f32 {
    (distance + 0.5).clamp(0., 1.)
}

/// The icon for `state` in `color`, as an image `img` draws: the status
/// item's bitmap, its black taken for `color`, the way AppKit draws a
/// template image.
fn icon_image(state: IconState, color: Hsla) -> Arc<Image> {
    let tint = Rgba::from(color);
    let rgb = [tint.r, tint.g, tint.b].map(|channel| (channel * 255.).round() as u8);
    let mut pixels = icon_rgba(state);
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel[..3].copy_from_slice(&rgb);
        pixel[3] = (f32::from(pixel[3]) * tint.a).round() as u8;
    }
    Arc::new(Image::from_bytes(ImageFormat::Png, png(&pixels, ICON_PX)))
}

/// `rgba` — straight RGBA, `side` pixels square — as a PNG file, for
/// gpui's image loader: one IDAT of stored deflate blocks. Nothing is
/// compressed, which a 5 KB icon on one screen of the harness does not
/// miss.
fn png(rgba: &[u8], side: u32) -> Vec<u8> {
    let (rows, row) = (side as usize, side as usize * 4);
    // Each row behind its filter byte, 0: none.
    let mut raw = Vec::with_capacity(rgba.len() + rows);
    for y in 0..rows {
        raw.push(0);
        raw.extend_from_slice(&rgba[y * row..(y + 1) * row]);
    }
    // zlib: deflate with a 32 KB window and no dictionary, the stored
    // blocks (the last one marked, an empty one if there is nothing), and
    // the Adler-32 of what they hold.
    let mut zlib = vec![0x78, 0x01];
    let mut blocks = raw.chunks(STORED_MAX).peekable();
    loop {
        let block = blocks.next().unwrap_or_default();
        let last = blocks.peek().is_none();
        let len = u16::try_from(block.len()).unwrap_or(u16::MAX);
        zlib.push(u8::from(last));
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&(!len).to_le_bytes());
        zlib.extend_from_slice(block);
        if last {
            break;
        }
    }
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&side.to_be_bytes());
    header.extend_from_slice(&side.to_be_bytes());
    // Eight bits a sample, RGBA, deflate, row filters, no interlace.
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    let mut file = b"\x89PNG\r\n\x1a\n".to_vec();
    for (kind, data) in [
        (b"IHDR", header.as_slice()),
        (b"IDAT", zlib.as_slice()),
        (b"IEND", &[][..]),
    ] {
        let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
        file.extend_from_slice(&len.to_be_bytes());
        file.extend_from_slice(kind);
        file.extend_from_slice(data);
        file.extend_from_slice(&crc32(&[&kind[..], data]).to_be_bytes());
    }
    file
}

/// The CRC-32 a PNG chunk ends with (ISO-HDLC: reflected, polynomial
/// `0xEDB88320`), of `parts` one after the other.
fn crc32(parts: &[&[u8]]) -> u32 {
    let mut crc = !0u32;
    for &byte in parts.iter().flat_map(|part| part.iter()) {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// The Adler-32 a zlib stream ends with.
fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checksums_are_the_ones_png_and_zlib_name() {
        // The standard check values, and the CRC every PNG ends with.
        assert_eq!(crc32(&[b"123456789"]), 0xCBF4_3926);
        assert_eq!(crc32(&[b"1234", b"56789"]), 0xCBF4_3926, "parts run on");
        assert_eq!(crc32(&[b"IEND"]), 0xAE42_6082);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(adler32(&[]), 1);
    }

    /// The chunks of a PNG file, as (kind, data), each checked against its
    /// CRC.
    fn png_chunks(file: &[u8]) -> Vec<(String, Vec<u8>)> {
        assert_eq!(&file[..8], b"\x89PNG\r\n\x1a\n");
        let mut found = Vec::new();
        let mut at = 8;
        while at < file.len() {
            let len = u32::from_be_bytes(file[at..at + 4].try_into().unwrap()) as usize;
            let kind = &file[at + 4..at + 8];
            let data = &file[at + 8..at + 8 + len];
            let crc = u32::from_be_bytes(file[at + 8 + len..at + 12 + len].try_into().unwrap());
            assert_eq!(crc, crc32(&[kind, data]), "{}", String::from_utf8_lossy(kind));
            found.push((String::from_utf8_lossy(kind).into_owned(), data.to_vec()));
            at += 12 + len;
        }
        assert_eq!(at, file.len());
        found
    }

    #[test]
    fn an_icon_is_a_png_whose_rows_are_the_bitmap_as_it_is() {
        let rgba = icon_rgba(IconState::Care);
        let file = png(&rgba, ICON_PX);
        let chunks = png_chunks(&file);
        let kinds: Vec<&str> = chunks.iter().map(|(kind, _)| kind.as_str()).collect();
        assert_eq!(kinds, ["IHDR", "IDAT", "IEND"]);
        let header = &chunks[0].1;
        assert_eq!(&header[..4], &ICON_PX.to_be_bytes());
        assert_eq!(&header[4..8], &ICON_PX.to_be_bytes());
        assert_eq!(&header[8..], &[8, 6, 0, 0, 0], "8-bit RGBA, not interlaced");
        // One stored block, the last: its length and complement, then each
        // row behind a zero filter byte, then the Adler-32 of them.
        let zlib = &chunks[1].1;
        assert_eq!(&zlib[..3], &[0x78, 0x01, 1]);
        let row = ICON_PX as usize * 4;
        let raw: Vec<u8> = rgba
            .chunks(row)
            .flat_map(|pixels| std::iter::once(0).chain(pixels.iter().copied()))
            .collect();
        let len = u16::try_from(raw.len()).unwrap();
        assert_eq!(&zlib[3..5], &len.to_le_bytes());
        assert_eq!(&zlib[5..7], &(!len).to_le_bytes());
        assert_eq!(&zlib[7..zlib.len() - 4], raw.as_slice());
        assert_eq!(&zlib[zlib.len() - 4..], &adler32(&raw).to_be_bytes());
        assert!(chunks[2].1.is_empty());
        // Nothing at all is still a stream: one empty last block.
        let empty = png_chunks(&png(&[], 0));
        assert_eq!(&empty[1].1[..7], &[0x78, 0x01, 1, 0, 0, 0xFF, 0xFF]);
    }

    #[test]
    fn the_window_draws_the_icon_in_the_colour_it_is_given_with_the_same_coverage() {
        let file = png(&icon_rgba(IconState::Linked), ICON_PX);
        let tinted = icon_image(IconState::Linked, gpui::white());
        assert_eq!(tinted.format(), ImageFormat::Png);
        assert_eq!(tinted.bytes().len(), file.len());
        // The same bitmap but for its colour: every pixel white, and the
        // alpha the template's.
        let template = icon_rgba(IconState::Linked);
        let zlib = &png_chunks(tinted.bytes())[1].1;
        let rows = &zlib[7..zlib.len() - 4];
        for (y, line) in rows.chunks(ICON_PX as usize * 4 + 1).enumerate() {
            assert_eq!(line[0], 0);
            for (x, pixel) in line[1..].chunks(4).enumerate() {
                let i = (y * ICON_PX as usize + x) * 4;
                assert_eq!(&pixel[..3], &[255, 255, 255]);
                assert_eq!(pixel[3], template[i + 3], "({x}, {y})");
            }
        }
    }
}
