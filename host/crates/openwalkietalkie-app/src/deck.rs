//! The Deck page (boards Main, DeckQuiet, DeckLight, DeckZh, NotLinked): the
//! device in the owner's hand — its picture with the face it shows, its
//! link, its battery, its firmware and the colour of its case — then what
//! needs attention, then the deck's own settings in its panels' names and
//! order.
//!
//! **Three layouts, by one rule** ([`layout`]). A deck is linked while the
//! service answers and the link is up; a degraded link still counts, and the
//! page says it is not answering. The service answering with no deck linked
//! is the not-linked page: when the deck was last seen, what to try, and the
//! one setting that is still the owner's to make. A service that does not
//! answer is the same page with its section saying why, as the installed app
//! can tell from macOS: not running, never set up, or held by macOS until
//! the owner allows it ([`no_service`]). The frame's strip, the Service page
//! and the status item say it in the same words ([`no_service_title`],
//! [`Shell::no_service_buttons`]).
//!
//! Everything shown is read from the mirror, the app's own case preference
//! and the two facts other pages read for it (sessions on an old helper, a
//! firmware update). Every control sends a `DeviceSet` and redraws from the
//! mirror the service answers with; the one thing the page keeps is the stop
//! a level was moved to, shown until the deck agrees or two seconds pass
//! ([`level_shown`]), or the thumb would lag the pointer by a round trip.
//!
//! The words are pure functions of those inputs and a language, so a test
//! reads every row in every state without a window.
use crate::app;
use crate::art::{self, Device};
use crate::prefs::Case;
use crate::present::{self, LoginItem};
use crate::shell::{Screen, Shell};
use crate::shots::Scene;
use crate::style::{palette, Palette};
use crate::ui::{self, ic, Btn, Level, PopupButton, Row, Segmented, Switch, Type as _};
use deck_gui_core::attention::{attention, Attention, Extras};
use deck_gui_core::i18n::Language;
use deck_gui_core::UiAction;
use deck_model::{
    DeviceState, FaceEntry, Link, LinkDrop, ServiceState, SetKey, Snapshot, Template, Tone,
    Transport, Ts, LINK_DROP_WINDOW,
};
use gpui::{div, prelude::*, px, AnyElement, App, Context, Div, Subscription, Window};
use gpui_component::menu::PopupMenuItem;
use gpui_component::{h_flex, v_flex};
use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::time::Duration;

/// The deck's column: its label and its card, beside the column of
/// sections.
const DECK_COLUMN: f32 = 280.;
/// How wide the deck is drawn in its card: wide enough for its plate to
/// carry the face's title.
const DECK_WIDTH: f32 = 164.;
/// The deck's card pads its rows a little less than a page's cards do.
const DECK_ROW_PAD: f32 = 14.;
/// Under the stage's caption: the design's 13, and one more for the card's
/// rule, which the card draws over its child's last pixel where the
/// design's stage has it as a border of its own.
const STAGE_BOTTOM: f32 = 14.;
/// Between the two columns, and between the sections of a column.
const GAP: f32 = 20.;

/// The deck's own ranges (`SetKey::accepts`): speaker and mic gain run from
/// 0 to 10, brightness from 1 to 5.
const SPEAKER: (u16, u16) = (0, 10);
const MIC: (u16, u16) = (0, 10);
const BRIGHTNESS: (u16, u16) = (1, 5);

/// The link's details: the dialog's id, its width, and how tall its list of
/// drops grows before it scrolls (an hour can hold sixty). gpui draws no
/// scroll bar, so the height is fifteen and a half of the list's 18 px
/// lines: a list that scrolls shows half a line at its foot, and that cut
/// line is what says there are more below.
const LINK_DETAILS: &str = "link-details";
const LINK_DETAILS_WIDTH: f32 = 420.;
const DROPS_HEIGHT: f32 = 279.;

/// The harness's scenes that are this page's.
const DECK_SCENES: [Scene; 8] = [
    Scene::Main,
    Scene::DeckQuiet,
    Scene::DeckZh,
    Scene::NotLinked,
    Scene::ServiceDown,
    Scene::NotInstalled,
    Scene::LinkDetails,
    Scene::DeckUnknown,
];

// ---- what the page says, as pure functions ---------------------------------

/// Which of its three layouts the page is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// A deck is linked: its rows, and its settings.
    Linked,
    /// The service answers and no deck is linked.
    NotLinked,
    /// The service does not answer.
    NoService,
}

/// The page's layout: linked while the service answers and the link is up
/// (a link that is up and silent is still linked), not linked while the
/// service answers with the link down, and neither while it does not
/// answer, whatever its last mirror said.
pub fn layout(connected: bool, link: Link) -> Layout {
    match (connected, link) {
        (false, _) => Layout::NoService,
        (true, Link::Offline) => Layout::NotLinked,
        (true, Link::Connected | Link::Degraded) => Layout::Linked,
    }
}

/// Why the service is not answering, as far as the window can tell: the
/// status item's model says it too, so it is `deck_gui_core`'s.
pub use deck_gui_core::NoService;

/// Why the service does not answer, in the installed app run for real
/// (`installed`), by its launch agent as macOS reports it: not registered
/// is never set up (by the rule first run opens by), waiting for approval
/// is held by macOS until the owner allows it in Login Items, and anything
/// else is set up and not running. Any other run cannot tell, and says it
/// is not running: it never offers to set the owner's service up.
pub fn no_service(installed: bool, agent: LoginItem) -> NoService {
    if !installed {
        return NoService::NotRunning;
    }
    match agent {
        LoginItem::NotRegistered | LoginItem::NotFound => NoService::NotInstalled,
        LoginItem::RequiresApproval => NoService::AwaitingApproval,
        LoginItem::Enabled | LoginItem::Unknown(_) => NoService::NotRunning,
    }
}

/// What a page calls a service that does not answer, `why` it does not:
/// the Deck page's section, the frame's strip and the Service page's row
/// all say this.
pub fn no_service_title(why: NoService, language: Language) -> &'static str {
    match why {
        NoService::NotRunning => language.text("The background service is not running"),
        NoService::NotInstalled => language.text("The background service is not installed"),
        NoService::AwaitingApproval => {
            language.text("The background service is waiting for your approval")
        }
    }
}

/// The Link row's word and its tone: how the deck is linked, a link that is
/// up and silent, a service waiting for a deck, or no service at all.
pub fn link_word(layout: Layout, device: &DeviceState, language: Language) -> (&'static str, Tone) {
    match layout {
        Layout::NoService => (language.text("Not linked"), Tone::Gray),
        Layout::NotLinked => (language.text("Waiting"), Tone::Gray),
        Layout::Linked if device.link == Link::Degraded => {
            (language.text("Not answering"), Tone::Amber)
        }
        Layout::Linked => match device.transport {
            Some(Transport::Bluetooth) => (language.text("Bluetooth"), Tone::Green),
            Some(Transport::Usb) => (language.text("Cable"), Tone::Green),
            // A service that predates the transport, or a deck that has
            // not said yet.
            None => (language.text("Linked"), Tone::Green),
        },
    }
}

/// The Battery row's figure: `85%`, `85% · charging` while the deck says it
/// is charging, or a dash while it has not said (it sends an unknown charge
/// as 0, which the service keeps as none).
pub fn battery_word(battery: Option<u8>, charging: Option<bool>, language: Language) -> String {
    let Some(percent) = battery else {
        return "\u{2014}".to_string();
    };
    let figure = language.format("{0}%", &[&percent.to_string()]);
    if charging == Some(true) {
        format!("{figure}{}", language.text(" · charging"))
    } else {
        figure
    }
}

/// The Firmware row: what the linked deck runs, or what the last deck ran
/// while none is linked, as `0.13.0 · Rev B`. `None` while it is not known,
/// and the row is left out.
pub fn firmware_word(layout: Layout, device: &DeviceState) -> Option<String> {
    let (fw, board) = match layout {
        Layout::Linked => (device.fw.as_deref(), device.board.as_deref()),
        Layout::NotLinked | Layout::NoService => {
            let last = device.last.as_ref()?;
            (last.fw.as_deref(), last.board.as_deref())
        }
    };
    Some(present::fw_text(fw?, board)).filter(|text| !text.is_empty())
}

/// The strip the page reads the deck's faces from: the deck's own report
/// while it has made one, else the strip the service keeps.
pub fn strip(mirror: &Snapshot) -> &[FaceEntry] {
    if mirror.device.faces.is_empty() {
        &mirror.faces
    } else {
        &mirror.device.faces
    }
}

/// The face the deck has on its screen, when it has said which and the
/// strip has it.
pub fn face_shown<'a>(device: &DeviceState, strip: &'a [FaceEntry]) -> Option<&'a FaceEntry> {
    let face = device.face?;
    strip.iter().find(|entry| entry.f == face)
}

/// A face's own name in English, the name the deck gives a face it adds.
pub const fn face_label(face: Template) -> &'static str {
    match face {
        Template::Agents => "Agents",
        Template::Macropad => "Macropad",
        Template::Usage => "Usage",
        Template::System => "System",
        Template::Timer => "Timer",
        Template::Arcade => "Arcade",
        Template::Settings => "Settings",
        Template::Omarchy => "Omarchy",
    }
}

/// What the page calls a face: the name the owner gave it, as it is, or —
/// for a face still called by its template's own name — that name in the
/// window's language.
pub fn face_name(entry: &FaceEntry, language: Language) -> String {
    let label = face_label(entry.t);
    if entry.n.is_empty() || entry.n == label {
        language.text(label).to_string()
    } else {
        entry.n.clone()
    }
}

/// When the deck was last linked, as the caption under a deck that is away
/// says it: `Last linked today, 09:41`. `None` when no deck ever was.
pub fn last_linked(device: &DeviceState, now: Ts, language: Language) -> Option<String> {
    let last = device.last.as_ref()?;
    let when = present::day_and_time(last.at, now, language);
    Some(language.format("Last linked {0}", &[&when]))
}

/// What the not-linked page says under "The background service is waiting
/// for your deck" once the service went quiet (`service.dormancy`): when it
/// stopped collecting from agent sessions, on this Mac's clock, or only that
/// it did when that was on an earlier day. `None` while it still collects,
/// from a service with no dormancy (one that predates it, or a test
/// service), and when no deck was ever linked, which gives it no time to
/// say.
pub fn stopped_collecting(service: &ServiceState, now: Ts, language: Language) -> Option<String> {
    let since = service.dormancy?.dormant_since?;
    let (year, month, day, hour, minute) = present::local_clock(since);
    let (now_year, now_month, now_day, _, _) = present::local_clock(now);
    Some(if (year, month, day) == (now_year, now_month, now_day) {
        language.format(
            "It stopped collecting from your agent sessions at {0},\n\
             ten minutes after the link dropped.",
            &[&format!("{hour:02}:{minute:02}")],
        )
    } else {
        language
            .text(
                "It stopped collecting from your agent sessions\n\
                 ten minutes after the link dropped.",
            )
            .to_string()
    })
}

/// The caption under the deck: the face it shows, `Linked` while it has not
/// said which, and while it is away when it was last linked (nothing when
/// it never was).
pub fn caption(
    layout: Layout,
    device: &DeviceState,
    strip: &[FaceEntry],
    now: Ts,
    language: Language,
) -> Option<String> {
    match layout {
        Layout::Linked => Some(match face_shown(device, strip) {
            Some(face) => language.format("Showing the {0} face", &[&face_name(face, language)]),
            None => language.text("Linked").to_string(),
        }),
        Layout::NotLinked | Layout::NoService => last_linked(device, now, language),
    }
}

/// Idle home's four choices, in seconds, as the deck offers them.
pub const IDLE_HOME: [u16; 4] = [0, 30, 60, 120];

/// The choice the deck shows for `seconds`: the first step at or above it,
/// the way it rounds a value it has no step for.
pub fn idle_home_at(seconds: u16) -> usize {
    ui::step_at(seconds, &IDLE_HOME)
}

/// The choice at `at` of [`IDLE_HOME`], in the deck's own words.
pub fn idle_home_word(at: usize, language: Language) -> &'static str {
    match at {
        0 => language.text("Never"),
        1 => language.text("30 s"),
        2 => language.text("1 min"),
        _ => language.text("2 min"),
    }
}

/// How long a level shows the stop it sent without the deck agreeing.
pub const LEVEL_HOLD: Duration = Duration::from_millis(2_000);

/// A stop a level was moved to and sent, and when.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sent {
    pub value: u16,
    pub at: Ts,
}

impl Sent {
    /// Whether it is still shown at `now`: until [`LEVEL_HOLD`] has passed.
    pub fn holds(self, now: Ts) -> bool {
        u128::from(now.millis().saturating_sub(self.at.millis())) < LEVEL_HOLD.as_millis()
    }
}

/// The stop a level shows: the one it sent while that still holds, else the
/// deck's own setting as the mirror has it.
pub fn level_shown(mirror: u16, sent: Option<Sent>, now: Ts) -> u16 {
    sent.filter(|sent| sent.holds(now))
        .map_or(mirror, |sent| sent.value)
}

/// The link's drops of the hour up to `now`, newest first, by the window
/// the attention list counts them in.
pub fn recent_drops(device: &DeviceState, now: Ts) -> Vec<&LinkDrop> {
    let window = u64::try_from(LINK_DROP_WINDOW.as_millis()).unwrap_or(u64::MAX);
    let mut drops: Vec<&LinkDrop> = device
        .drops
        .iter()
        .filter(|drop| now.millis().saturating_sub(drop.at.millis()) <= window)
        .collect();
    drops.sort_by_key(|drop| Reverse(drop.at));
    drops
}

/// Why the link dropped, in words: the service's reason, or the reason as it
/// came when this window has no words for it.
pub fn drop_reason(why: &str, language: Language) -> &str {
    match why {
        "read" => language.text("read error"),
        "write" => language.text("write error"),
        "write_stall" => language.text("write stalled"),
        "removed" => language.text("unplugged or out of range"),
        "misses" => language.text("stopped answering"),
        other => other,
    }
}

/// One drop as the link details list it: `09:58 · stopped answering`, on
/// this Mac's clock.
pub fn drop_line(drop: &LinkDrop, language: Language) -> String {
    let (_, _, _, hour, minute) = present::local_clock(drop.at);
    let time = format!("{hour:02}:{minute:02}");
    language.format("{0} · {1}", &[&time, drop_reason(&drop.why, language)])
}

/// What an attention card offers beside its words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Remedy {
    /// **Details**: the link's drops.
    Details,
    /// **Open Service**: the page that can do something about it.
    OpenService,
}

/// One attention card: what is wrong, what to do, and the button, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub sub: String,
    pub remedy: Option<Remedy>,
}

/// The card for `item`. `port` is the service's port, which a degraded
/// service's words name.
pub fn notice(item: &Attention, port: u16, language: Language) -> Notice {
    let words = |title: String, sub: &str, remedy| Notice {
        title,
        sub: sub.to_string(),
        remedy,
    };
    match item {
        Attention::LinkDrops { count } => words(
            language.format(
                "The link reconnected {0} times in the last hour",
                &[&count.to_string()],
            ),
            language.text("Try the cable. If it goes on, send the support file."),
            Some(Remedy::Details),
        ),
        Attention::NotAnswering => words(
            language.text("The deck is not answering").to_string(),
            language.text("It is linked but silent. Move it closer or use the cable."),
            None,
        ),
        // The service's own words for what is wrong with it, as the window
        // has always said them.
        Attention::ServiceDegraded { reason } => {
            let service = ServiceState {
                degraded: Some(reason.clone()),
                port,
                ..ServiceState::default()
            };
            let sub = present::degraded_banner_localized(&service, language)
                .unwrap_or_else(|| language.format("The service is degraded: {0}.", &[reason]));
            words(
                language.text("The service needs attention").to_string(),
                &sub,
                Some(Remedy::OpenService),
            )
        }
        Attention::StaleHelpers { count: 1 } => words(
            language
                .text("1 Claude session uses an old helper")
                .to_string(),
            language.text("Restart that session."),
            None,
        ),
        Attention::StaleHelpers { count } => words(
            language.format(
                "{0} Claude sessions use an old helper",
                &[&count.to_string()],
            ),
            language.text("Restart those sessions."),
            None,
        ),
        Attention::FirmwareUpdate { version } => words(
            language.format(
                "Deck firmware {0} is available",
                &[&present::fw_text(version, None)],
            ),
            language.text("Install it by cable from Service."),
            Some(Remedy::OpenService),
        ),
    }
}

// ---- the pane ----------------------------------------------------------------

/// The page's own state, which the shell owns: the stops the levels were
/// moved to, and the answer a harness scene gave for why the service is
/// not answering.
pub struct DeckPane {
    /// Each level's last stop sent, until the deck agrees or it no longer
    /// holds.
    sent: BTreeMap<SetKey, Sent>,
    /// A harness scene's [`NoService`], in place of this run's own.
    staged: Option<NoService>,
}

impl DeckPane {
    /// The pane, and the subscriptions its text fields need kept alive: it
    /// has no text field.
    pub(crate) fn new(
        _window: &mut Window,
        _cx: &mut Context<Shell>,
    ) -> (DeckPane, Vec<Subscription>) {
        let pane = DeckPane {
            sent: BTreeMap::new(),
            staged: None,
        };
        (pane, Vec::new())
    }
}

impl Shell {
    /// Nothing to ask: everything the page draws is the mirror's, and the
    /// two facts it borrows are read by the pages they belong to.
    pub(crate) fn refresh_deck(
        &mut self,
        _force: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    /// The page. It is never dimmed as a whole: a service that is not
    /// answering is one of its layouts, with its own buttons.
    pub(crate) fn render_deck(&self, _dim: bool, cx: &Context<Self>) -> AnyElement {
        let p = *palette(cx);
        let layout = layout(self.state.connected(), self.state.mirror().device.link);
        let header = ui::Header::new(
            self.tr(Screen::Deck.label()),
            self.tr("The device in your hand: its link, its battery, its settings."),
        );
        let sections = match layout {
            Layout::Linked => self.deck_settings(&p, cx),
            Layout::NotLinked | Layout::NoService => self.deck_away(layout, &p, cx),
        };
        let columns = h_flex()
            .w_full()
            .items_start()
            .gap(px(GAP))
            .child(self.deck_card(layout, &p, cx))
            .child(sections);
        let body = v_flex().gap(px(GAP)).child(header).child(columns);
        ui::page("deck", &self.page_scroll, self.content_width, body).into_any_element()
    }

    /// The page's dialog `id` and its width: the link's details.
    pub(crate) fn deck_dialog(
        &self,
        id: &'static str,
        cx: &Context<Self>,
    ) -> Option<(AnyElement, f32)> {
        (id == LINK_DETAILS).then(|| (self.render_link_details(cx), LINK_DETAILS_WIDTH))
    }

    /// The link's details keep nothing of their own to let go of.
    pub(crate) fn deck_dialog_closed(&mut self, _id: &'static str, _cx: &mut Context<Self>) {}

    /// Nothing lives outside the render: the page has no text field.
    pub(crate) fn localize_deck(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    /// A level the deck now agrees with shows the deck's setting again.
    pub(crate) fn on_deck_event(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        let settings = self.state.mirror().settings;
        self.deck
            .sent
            .retain(|key, sent| settings.get(*key) != sent.value);
    }

    /// A stop the deck never agreed with is let go of once it no longer
    /// holds; the shell redraws after the tick.
    pub(crate) fn tick_deck(&mut self, _beat: u64, _window: &mut Window, _cx: &mut Context<Self>) {
        let now = present::now();
        self.deck.sent.retain(|_, sent| sent.holds(now));
    }

    /// The page's scenes. The mirror is the scene's fixture; the page adds
    /// why the service is not answering (a harness run is never the
    /// installed app, so it could not tell "not installed" by itself) and
    /// opens the link's details for `link-details`.
    pub(crate) fn stage_deck(
        &mut self,
        scene: Scene,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.deck.sent.clear();
        self.deck.staged = match scene {
            Scene::ServiceDown => Some(NoService::NotRunning),
            Scene::NotInstalled => Some(NoService::NotInstalled),
            _ => None,
        };
        if !DECK_SCENES.contains(&scene) {
            return false;
        }
        self.go(Screen::Deck, window, cx);
        if scene == Scene::LinkDetails {
            self.open_link_details(window, cx);
        }
        true
    }

    /// Open the link's details: the menu's **Show Link Problem…** and the
    /// link card's **Details**.
    pub(crate) fn open_link_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_dialog(LINK_DETAILS, window);
        cx.notify();
    }

    // ---- the deck's card ---------------------------------------------------

    /// The deck's column: the picture on its stage with its caption, then
    /// Link, Battery (linked only), Firmware (when known) and Case.
    fn deck_card(&self, layout: Layout, p: &Palette, cx: &Context<Self>) -> Div {
        let language = self.language();
        let mirror = self.state.mirror();
        let device = &mirror.device;
        let linked = layout == Layout::Linked;
        let case = self.prefs(cx).case();
        // A deck that is away shows nothing on its screen, whatever it last
        // showed.
        let face = face_shown(device, strip(mirror))
            .map(|entry| entry.t)
            .filter(|_| linked);
        let picture = Device::new(case, face, DECK_WIDTH)
            .dim(!linked)
            .when_some(face, |picture, face| {
                picture.title(self.tr(art::plate_title(face)))
            });
        let caption = caption(layout, device, strip(mirror), present::now(), language);
        // The deck's box as the design lays it out, its height rounded to
        // the pixel (264 at 164 wide).
        let deck_box = art::DeviceGeometry::at(DECK_WIDTH).height.round();
        // The stage's fill reaches the card's top corners, so it rounds them
        // itself, inside the card's 1 px rim.
        let stage = v_flex()
            .w_full()
            .items_center()
            .gap(px(12.))
            .pt(px(18.))
            .pb(px(STAGE_BOTTOM))
            .rounded_t(px(11.))
            .bg(p.stage)
            .child(div().flex_none().h(px(deck_box)).child(picture))
            .children(caption.map(|caption| div().ts(12., 16.).text_color(p.text2).child(caption)));
        let (word, tone) = link_word(layout, device, language);
        let mut rows = vec![
            stage.into_any_element(),
            Row::new()
                .pad(DECK_ROW_PAD)
                .title(self.tr("Link"))
                .right(ui::status(word, tone, p))
                .into_any_element(),
        ];
        if linked {
            rows.push(self.battery_row(device, p).into_any_element());
        }
        if let Some(firmware) = firmware_word(layout, device) {
            rows.push(
                Row::new()
                    .pad(DECK_ROW_PAD)
                    .title(self.tr("Firmware"))
                    .right(ui::mono(firmware, p.text2))
                    .into_any_element(),
            );
        }
        let chosen = Case::ALL
            .iter()
            .position(|c| *c == case)
            .unwrap_or_default();
        let colours = Segmented::new(
            "deck-case",
            vec![self.tr("White").into(), self.tr("Black").into()],
            chosen,
        )
        .on_select(cx.listener(|this, at: &usize, _, cx| {
            if let Some(case) = Case::ALL.get(*at).copied() {
                this.update_prefs(cx, |prefs| prefs.set_case(case));
            }
        }));
        rows.push(
            Row::new()
                .pad(DECK_ROW_PAD)
                .title(self.tr("Case"))
                .right(colours)
                .into_any_element(),
        );
        v_flex()
            .flex_none()
            .w(px(DECK_COLUMN))
            .gap(px(8.))
            .child(ui::label(self.tr("Your deck"), p))
            .child(ui::card(rows, p))
    }

    /// The Battery row: the glyph and the figure, or a dash while the deck
    /// has not said.
    fn battery_row(&self, device: &DeviceState, p: &Palette) -> Row {
        let figure = battery_word(device.battery, device.charging, self.language());
        let row = Row::new().pad(DECK_ROW_PAD).title(self.tr("Battery"));
        match device.battery {
            Some(percent) => row
                .right(art::battery(percent, p))
                .right(div().ts(13., 18.).medium().tnum().child(figure)),
            None => row.right(div().ts(13., 18.).text_color(p.text2).child(figure)),
        }
    }

    // ---- the sections --------------------------------------------------------

    /// What needs attention, a card each, in the attention list's order:
    /// `None` when nothing does.
    fn deck_attention(&self, p: &Palette, cx: &Context<Self>) -> Option<Div> {
        let mirror = self.state.mirror();
        let extras = Extras {
            stale_helpers: self.integrations.stale_helpers(),
            firmware: self.service.firmware_available(),
        };
        let items = attention(mirror, self.state.connected(), present::now(), &extras);
        if items.is_empty() {
            return None;
        }
        let language = self.language();
        let cards = items.iter().enumerate().map(|(i, item)| {
            let notice = notice(item, mirror.service.port, language);
            let row = Row::new()
                .lead(ui::bare(ui::icon(ic::DANGER, 18., p.warn)))
                .title(notice.title)
                .sub(notice.sub);
            let row = match notice.remedy {
                Some(Remedy::Details) => row.right(
                    Btn::new(("deck-attention", i), self.tr("Details")).on_click(
                        cx.listener(|this, _, window, cx| this.open_link_details(window, cx)),
                    ),
                ),
                Some(Remedy::OpenService) => row.right(
                    Btn::new(("deck-attention", i), self.tr("Open Service")).on_click(
                        cx.listener(|this, _, window, cx| this.go(Screen::Service, window, cx)),
                    ),
                ),
                None => row,
            };
            ui::attention(row, p)
        });
        let cards = v_flex().gap(px(8.)).children(cards);
        Some(ui::section(self.tr("Needs attention"), cards, p))
    }

    /// The deck's own settings, in its panels' names and order: Volume and
    /// speech, Alerts, Display.
    fn deck_settings(&self, p: &Palette, cx: &Context<Self>) -> Div {
        let settings = self.state.mirror().settings;
        let switch = |id: &'static str, key: SetKey| {
            Switch::new(id, settings.get(key) != 0).on_toggle(cx.listener(
                move |this, on: &bool, window, cx| {
                    let value = u16::from(*on);
                    this.act(UiAction::DeviceSet { key, value }, window, cx);
                },
            ))
        };
        let volume = ui::card(
            vec![
                Row::new()
                    .title(self.tr("Speaker"))
                    .right(self.deck_level("deck-speaker", SetKey::Speaker, SPEAKER, cx))
                    .into_any_element(),
                Row::new()
                    .title(self.tr("Mic gain"))
                    .right(self.deck_level("deck-mic", SetKey::Mic, MIC, cx))
                    .into_any_element(),
                Row::new()
                    .title(self.tr("Auto-announce"))
                    .right(switch("deck-announce", SetKey::Announce))
                    .into_any_element(),
            ],
            p,
        );
        let alerts = ui::card(
            vec![
                Row::new()
                    .title(self.tr("Chime"))
                    .right(switch("deck-chime", SetKey::Chime))
                    .into_any_element(),
                Row::new()
                    .title(self.tr("Auto-jump"))
                    .right(switch("deck-autojump", SetKey::Autojump))
                    .into_any_element(),
            ],
            p,
        );
        let sizes = Segmented::new(
            "deck-text-size",
            vec![
                self.tr("Small").into(),
                self.tr("Medium").into(),
                self.tr("Large").into(),
            ],
            usize::from(settings.textsize.min(2)),
        )
        .on_select(cx.listener(|this, at: &usize, window, cx| {
            let value = u16::try_from(*at).unwrap_or_default();
            this.act(
                UiAction::DeviceSet {
                    key: SetKey::Textsize,
                    value,
                },
                window,
                cx,
            );
        }));
        let display = ui::card(
            vec![
                Row::new()
                    .title(self.tr("Brightness"))
                    .right(self.deck_level("deck-brightness", SetKey::Brightness, BRIGHTNESS, cx))
                    .into_any_element(),
                Row::new()
                    .title(self.tr("Idle home"))
                    .right(self.idle_home_popup(settings.idlehome, cx))
                    .into_any_element(),
                Row::new()
                    .title(self.tr("Text size"))
                    .right(sizes)
                    .into_any_element(),
                Row::new()
                    .title(self.tr("Language"))
                    .sub(self.tr("The app and the deck change together."))
                    .right(self.language_popup(true, cx))
                    .into_any_element(),
            ],
            p,
        );
        v_flex()
            .flex_1()
            .min_w_0()
            .gap(px(GAP))
            .children(self.deck_attention(p, cx))
            .child(ui::section(self.tr("Volume and speech"), volume, p))
            .child(ui::section(self.tr("Alerts"), alerts, p))
            .child(ui::section(self.tr("Display"), display, p))
    }

    /// The deck away, or the service: what is going on, the two things to
    /// try, the language (the one setting the owner can still make, and
    /// only through the service), and the note that the rest comes back.
    fn deck_away(&self, layout: Layout, p: &Palette, cx: &Context<Self>) -> Div {
        let absent = (layout == Layout::NoService).then(|| self.service_absence());
        let first = match absent {
            None => Row::new()
                .lead(ui::bare(ui::icon(ic::INFO, 18., p.text2)))
                .title(self.tr("The background service is waiting for your deck"))
                .when_some(
                    stopped_collecting(
                        &self.state.mirror().service,
                        present::now(),
                        self.language(),
                    ),
                    |row, line| row.sub(line),
                ),
            Some(why) => {
                // Not running is something wrong; never set up, or waiting
                // for the owner's approval, is the owner's turn.
                let mark = match why {
                    NoService::NotRunning => ui::icon(ic::DANGER, 18., p.warn),
                    NoService::NotInstalled | NoService::AwaitingApproval => {
                        ui::icon(ic::INFO, 18., p.text2)
                    }
                };
                let row = Row::new()
                    .lead(ui::bare(mark))
                    .title(no_service_title(why, self.language()))
                    .when(why == NoService::NotRunning, |row| {
                        row.sub(self.tr("The deck works only while it runs."))
                    });
                self.no_service_buttons(why, "deck-absent", true, cx)
                    .into_iter()
                    .fold(row, Row::right)
            }
        };
        let dark = Row::new()
            .lead(ui::bare(div()))
            .title(self.tr("If the screen is dark"))
            .sub(self.tr("Hold PTT, the long key on the left edge, for 2 seconds."));
        let pair = Row::new()
            .lead(ui::bare(div()))
            .title(self.tr("Cable or Bluetooth"))
            .sub(self.tr("Use the cable, or pair it again."))
            .right(
                Btn::new("deck-bluetooth", self.tr("Open Bluetooth Settings"))
                    .on_click(|_, _, cx: &mut App| cx.open_url(present::BLUETOOTH_SETTINGS)),
            );
        let (title, language_sub) = match absent {
            None => (
                self.tr("Not linked"),
                self.tr("The deck follows when it links."),
            ),
            Some(_) => (
                self.tr("Not running"),
                self.tr("Needs the background service."),
            ),
        };
        let what = ui::card(
            vec![
                first.into_any_element(),
                dark.into_any_element(),
                pair.into_any_element(),
            ],
            p,
        );
        let language = Row::new()
            .title(self.tr("Language"))
            .sub(language_sub)
            .right(self.language_popup(absent.is_none(), cx));
        v_flex()
            .flex_1()
            .min_w_0()
            .gap(px(GAP))
            .children(self.deck_attention(p, cx))
            .child(ui::section(title, what, p))
            .child(ui::section(
                self.tr("Still yours to set"),
                ui::card(vec![language.into_any_element()], p),
                p,
            ))
            .child(ui::hint(
                self.tr("Volume, alerts and display settings come back when the deck links."),
                p,
            ))
    }

    /// Why no service answers, as this run can tell: a harness scene's
    /// answer, or the service's launch agent as macOS last reported it
    /// ([`no_service`]). The Deck page, the frame's strip, the Service page
    /// and the status item all read it here, so they say the same.
    pub(crate) fn service_absence(&self) -> NoService {
        self.deck
            .staged
            .unwrap_or_else(|| no_service(app::launch().installed(), self.service.agent()))
    }

    /// What a page offers for a service that does not answer, `why` it does
    /// not: **Start** and **Open Log** for one that is set up and not
    /// running; **Set Up…** (first run, [`Shell::set_up_service`]) for one
    /// never set up; **Open Settings** at Login Items for one macOS holds
    /// for the owner's approval. `id` keeps one page's buttons apart from
    /// another's; `primary` draws the first as the primary.
    pub(crate) fn no_service_buttons(
        &self,
        why: NoService,
        id: &'static str,
        primary: bool,
        cx: &Context<Self>,
    ) -> Vec<Btn> {
        let first = |button: Btn| if primary { button.primary() } else { button };
        match why {
            NoService::NotRunning => vec![
                first(Btn::new((id, 0_usize), self.tr("Start")).on_click(
                    cx.listener(|this, _, window, cx| this.start_service(window, cx)),
                )),
                Btn::new((id, 1_usize), self.tr("Open Log"))
                    .on_click(|_, _, cx: &mut App| cx.open_with_system(&app::launchd_log())),
            ],
            NoService::NotInstalled => vec![first(
                Btn::new((id, 0_usize), self.tr("Set Up\u{2026}")).on_click(cx.listener(
                    |this, _, window, cx| this.set_up_service(window, cx),
                )),
            )],
            NoService::AwaitingApproval => vec![first(
                Btn::new((id, 0_usize), self.tr("Open Settings"))
                    .on_click(|_, _, cx: &mut App| cx.open_url(present::LOGIN_ITEMS_SETTINGS)),
            )],
        }
    }

    // ---- the controls --------------------------------------------------------

    /// One of the deck's levels, showing the stop it sent while that holds.
    fn deck_level(
        &self,
        id: &'static str,
        key: SetKey,
        (min, max): (u16, u16),
        cx: &Context<Self>,
    ) -> Level {
        let setting = self.state.mirror().settings.get(key);
        let shown = level_shown(setting, self.deck.sent.get(&key).copied(), present::now());
        Level::new(id, shown, min, max).on_change(cx.listener(
            move |this, value: &u16, window, cx| {
                this.set_deck_level(key, *value, window, cx);
            },
        ))
    }

    /// Send a level's new stop, and show it until the deck agrees.
    fn set_deck_level(
        &mut self,
        key: SetKey,
        value: u16,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let at = present::now();
        self.deck.sent.insert(key, Sent { value, at });
        self.act(UiAction::DeviceSet { key, value }, window, cx);
        cx.notify();
    }

    /// Idle home: the deck's four choices, showing the one it rounds its
    /// setting to.
    fn idle_home_popup(&self, seconds: u16, cx: &Context<Self>) -> PopupButton {
        let language = self.language();
        let at = idle_home_at(seconds);
        let shell = cx.entity();
        PopupButton::new("deck-idle-home", idle_home_word(at, language)).menu(
            move |mut menu, _, _| {
                for (i, value) in IDLE_HOME.into_iter().enumerate() {
                    let shell = shell.clone();
                    let item = PopupMenuItem::new(idle_home_word(i, language))
                        .checked(i == at)
                        .on_click(move |_, window, cx| {
                            shell.update(cx, |this, cx| {
                                let key = SetKey::Idlehome;
                                this.act(UiAction::DeviceSet { key, value }, window, cx);
                            });
                        });
                    menu = menu.item(item);
                }
                menu
            },
        )
    }

    /// The language, by each one's own name. The app and the deck change
    /// together; with no deck linked the service keeps it for the deck, and
    /// with no service it cannot be set here (`enabled`).
    fn language_popup(&self, enabled: bool, cx: &Context<Self>) -> PopupButton {
        let current = self.language();
        let shell = cx.entity();
        PopupButton::new("deck-language", current.name())
            .disabled(!enabled)
            .menu(move |mut menu, _, _| {
                for language in Language::ALL {
                    let shell = shell.clone();
                    let item = PopupMenuItem::new(language.name())
                        .checked(language == current)
                        .on_click(move |_, window, cx| {
                            shell.update(cx, |this, cx| this.choose_language(language, window, cx));
                        });
                    menu = menu.item(item);
                }
                menu
            })
    }

    // ---- the link's details --------------------------------------------------

    /// The drops of the last hour, newest first, each with its local time
    /// and its reason; **Open Service** and **Close**.
    fn render_link_details(&self, cx: &Context<Self>) -> AnyElement {
        let p = *palette(cx);
        let language = self.language();
        let lines: Vec<String> = recent_drops(&self.state.mirror().device, present::now())
            .into_iter()
            .map(|drop| drop_line(drop, language))
            .collect();
        let list = if lines.is_empty() {
            div()
                .text_color(p.text2)
                .child(ui::para(self.tr("No drops in the last hour.")))
                .into_any_element()
        } else {
            v_flex()
                .id("link-drops")
                .max_h(px(DROPS_HEIGHT))
                .overflow_y_scroll()
                .tnum()
                .text_color(p.text2)
                .children(lines.into_iter().map(|line| div().child(line)))
                .into_any_element()
        };
        let buttons = h_flex()
            .mt(px(16.))
            .gap(px(8.))
            .justify_end()
            .child(
                Btn::new("link-open-service", self.tr("Open Service")).on_click(cx.listener(
                    |this, _, window, cx| {
                        this.close_dialog(window, cx);
                        this.go(Screen::Service, window, cx);
                    },
                )),
            )
            .child(
                Btn::new("link-close", self.tr("Close"))
                    .primary()
                    .on_click(cx.listener(|this, _, window, cx| this.close_dialog(window, cx))),
            );
        v_flex()
            .p(px(20.))
            .gap(px(4.))
            .ts(13., 18.)
            .child(div().semibold().child(self.tr("Link problem")))
            .child(div().mt(px(8.)).child(list))
            .child(buttons)
            .into_any_element()
    }
}
