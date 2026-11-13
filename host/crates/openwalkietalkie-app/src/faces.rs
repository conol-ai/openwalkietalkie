//! The Faces & Keys page (boards FacesKeys, FacesKeysLight, FacesUsage): the
//! deck's face strip — the faces it slides through with SHIFT and the
//! wheel — and under it what the selected face does on the Mac: a
//! Macropad's keys ([`crate::keys`]), where the Usage face's bars come
//! from, the Agents face's demo sessions, the Arcade face's sound, or
//! nothing.
//!
//! **The strip is the deck's.** It is drawn from what the linked deck last
//! reported (`device.faces`), or, with the deck away, from what the service
//! last adopted (`faces`) ([`strip`]). A change — a face moved, removed or
//! added — is a whole new strip, built valid by the deck's own rules
//! ([`pinned`], [`addable`], [`free_id`], [`moved`], [`removed`],
//! [`added`]) and sent with `device.faces.set`. The service refuses a strip
//! those rules refuse ([`deck_model::strip_problem`], whose limits these
//! are) and adopts none, and the deck does not answer a strip: it reports
//! its own a moment later. So the page shows the deck's strip throughout and
//! waits for the deck to report the new one, eight seconds at most
//! ([`Pending`]); a deck that did not take it (its face manager was open)
//! is said so. Changing the strip needs the deck linked. A Macropad's keys
//! go with it ([`Forget`]): once the deck reports the strip without it, its
//! keys are written empty, so the service stops keeping them for a face that
//! is gone. The service keeps a strip and keys for each deck, so what the
//! page waits on is for one deck ([`StripWrites`]): another one in the hand
//! lets all of it go.
//!
//! **The Usage face's sources** are `usage.providers` in the service's
//! config, read with `config.get` and written whole ([`UsageConfig`]). The
//! Claude bars read the Claude Code login from the Keychain, so they stay
//! off until they are switched on here.
//!
//! The rules and the words are pure functions of the mirror and a language,
//! so a test reads them without a window.
use crate::art;
use crate::keys::KeysPane;
use crate::present;
use crate::shell::{Screen, Sheet, Shell};
use crate::shots::Scene;
use crate::style::{palette, Palette};
use crate::ui::{self, ic, Btn, Layout, Press, Roving, Row, Segmented, Switch, Type as _};
use deck_client::ClientError;
use deck_gui_core::admin::method_missing;
use deck_gui_core::i18n::Language;
use deck_gui_core::{ConfigValue, UiAction};
use deck_model::{FaceEntry, ServiceState, SetKey, Snapshot, Template, Ts};
use gpui::{
    div, prelude::*, px, AnyElement, Context, Div, ElementId, KeyDownEvent, Modifiers,
    SharedString, Subscription, Window,
};
use gpui_component::{h_flex, v_flex, StyledExt as _};
use serde_json::Value;
use std::time::Duration;

/// A strip holds eight faces at most; the deck then offers no "+".
pub const MAX_FACES: usize = deck_model::STRIP_MAX;

/// The deck keeps keys for three Macropad faces at most.
pub const MAX_MACROPADS: usize = deck_model::MACROPADS_MAX;

/// The deck's clocks are one per deck: one Timer face at most.
pub const MAX_TIMERS: usize = deck_model::TIMERS_MAX;

/// How long a strip that was sent waits for the deck to report it.
pub const PENDING_HOLD: Duration = Duration::from_secs(8);

/// The faces the deck's own gallery offers, in its order. Agents and
/// Settings are always on the strip, at its ends, and never added.
pub const GALLERY: [Template; 6] = [
    Template::Omarchy,
    Template::Macropad,
    Template::Usage,
    Template::Arcade,
    Template::System,
    Template::Timer,
];

/// The usage providers the Usage face's switches name.
pub const CLAUDE: &str = "claude";
pub const CODEX: &str = "codex";

/// The config key of the Usage face's sources.
pub const PROVIDERS: &str = "usage.providers";

/// The dialog that adds a face.
const ADD_FACE: &str = "add-face";

/// The strip's tiles: the plate, its corner, and the room between two.
const TILE: f32 = 79.;
const TILE_RADIUS: f32 = 12.;
const TILE_GAP: f32 = 14.;
/// A tile's column is as wide as its plate, or as its name with the pin
/// when that is wider (as the design's tile, a column with no set width,
/// is): the Japanese エージェント and its pin take 92. Up to a plate and a
/// gap.
const COLUMN_MAX: f32 = TILE + TILE_GAP;
/// A full strip (eight columns: the faces, and **Add a face** while one
/// more fits) spreads its columns across the card, at least this far
/// apart: 14 while every column is a plate wide, as drawn (8 × 79 + 7 × 14
/// fills the card's 730), and about 12 with one column 92 wide. Either
/// clears a selected tile's ring.
const FULL_GAP: f32 = 8.;
/// Columns on a full strip.
const FULL_COLUMNS: usize = MAX_FACES;
/// Under the tiles: the design's 12, and one more for the card's rule,
/// drawn over the row's last pixel where the design's buttons row has a
/// border of its own.
const TILES_BOTTOM: f32 = 13.;
/// Between the page's blocks, and between the two columns of a panel.
const GAP: f32 = 20.;
/// A face's settings, beside its plate.
const SETTINGS_WIDTH: f32 = 444.;
/// The plate "On the deck".
const PREVIEW: f32 = 116.;
/// The page while the service is not answering.
const DIM: f32 = 0.55;
/// Omarchy is a face for Linux desktops; this Mac does not add one.
const LINUX: bool = cfg!(target_os = "linux");

/// The harness's scenes that are this page's: the face each selects, and
/// what the service says of Accessibility in it.
const SCENES: [(Scene, Template, &str); 6] = [
    (Scene::FacesKeys, Template::Macropad, "granted"),
    (Scene::FacesUsage, Template::Usage, "granted"),
    (Scene::FacesFull, Template::Arcade, "granted"),
    (Scene::FacesNoAccess, Template::Macropad, "denied"),
    (Scene::FacesAdd, Template::Macropad, "granted"),
    (Scene::FacesRemove, Template::Macropad, "granted"),
];

/// The key the boards select: the fifth, the screenshot.
const STAGED_KEY: usize = 4;

/// The service's config as the boards have it (`usage.providers` Codex
/// alone, as the service's default is).
const CONFIG_FIXTURE: &str =
    include_str!("../../deck-gui-core/tests/fixtures/daemon/config_get_sources.json");

// ---- the strip, as pure rules ----------------------------------------------

/// Whether a face stays where it is: Agents first and Settings last, as the
/// deck's own face manager keeps them.
pub const fn pinned(face: Template) -> bool {
    matches!(face, Template::Agents | Template::Settings)
}

/// Whether the face at `at` can change places with the one before it:
/// neither is pinned.
pub fn can_move_left(list: &[FaceEntry], at: usize) -> bool {
    at > 0 && at < list.len() && !pinned(list[at].t) && !pinned(list[at - 1].t)
}

/// Whether the face at `at` can change places with the one after it.
pub fn can_move_right(list: &[FaceEntry], at: usize) -> bool {
    at + 1 < list.len() && !pinned(list[at].t) && !pinned(list[at + 1].t)
}

/// Whether the face at `at` can be taken off the strip: any but a pinned
/// one.
pub fn can_remove(list: &[FaceEntry], at: usize) -> bool {
    list.get(at).is_some_and(|face| !pinned(face.t))
}

/// The faces `list` can take, in the gallery's order: none once it holds
/// eight; a Macropad while it has fewer than three, a Timer while it has
/// none; Omarchy only on Linux; never Agents or Settings.
pub fn addable(list: &[FaceEntry], linux: bool) -> Vec<Template> {
    if list.len() >= MAX_FACES {
        return Vec::new();
    }
    let count = |face: Template| list.iter().filter(|entry| entry.t == face).count();
    GALLERY
        .into_iter()
        .filter(|&face| match face {
            Template::Omarchy => linux,
            Template::Macropad => count(face) < MAX_MACROPADS,
            Template::Timer => count(face) < MAX_TIMERS,
            Template::Agents | Template::Settings => false,
            Template::Usage | Template::Arcade | Template::System => true,
        })
        .collect()
}

/// A face's own id, the one the deck gives it when it is free: the order
/// of the deck's faces, from 1.
pub const fn default_id(face: Template) -> u8 {
    match face {
        Template::Agents => 1,
        Template::Omarchy => 2,
        Template::Macropad => 3,
        Template::Usage => 4,
        Template::Settings => 5,
        Template::Arcade => 6,
        Template::System => 7,
        Template::Timer => 8,
    }
}

/// The id a new `face` gets on `list`: its own while no face has it, else
/// the lowest free one. `None` when all eight are taken.
pub fn free_id(list: &[FaceEntry], face: Template) -> Option<u8> {
    let free = |id: u8| list.iter().all(|entry| entry.f != id);
    let own = default_id(face);
    if free(own) {
        return Some(own);
    }
    (1..=MAX_FACES as u8).find(|&id| free(id))
}

/// Which way a face moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// The way a key moves the selected face along the strip: Option-Left and
/// Option-Right, as **Move Left** and **Move Right** do. Any other key, or
/// an arrow with another modifier, is `None`.
pub fn strip_move(key: &str, modifiers: &Modifiers) -> Option<Side> {
    let option_alone = modifiers.alt
        && !(modifiers.control || modifiers.shift || modifiers.platform || modifiers.function);
    match key {
        "left" if option_alone => Some(Side::Left),
        "right" if option_alone => Some(Side::Right),
        _ => None,
    }
}

/// `list` with the face at `at` moved one place to `side`: as it was when
/// the move is not allowed.
pub fn moved(list: &[FaceEntry], at: usize, side: Side) -> Vec<FaceEntry> {
    let mut list = list.to_vec();
    match side {
        Side::Left if can_move_left(&list, at) => list.swap(at - 1, at),
        Side::Right if can_move_right(&list, at) => list.swap(at, at + 1),
        _ => {}
    }
    list
}

/// `list` without the face at `at`: as it was when that face cannot go.
pub fn removed(list: &[FaceEntry], at: usize) -> Vec<FaceEntry> {
    let mut list = list.to_vec();
    if can_remove(&list, at) {
        list.remove(at);
    }
    list
}

/// `list` with a new `face`, as the deck adds one: before Settings (at the
/// end when there is none), with [`free_id`]'s id and the face's own name.
/// As it was when the strip cannot take it.
pub fn added(list: &[FaceEntry], face: Template) -> Vec<FaceEntry> {
    let mut list = list.to_vec();
    if !addable(&list, true).contains(&face) {
        return list;
    }
    let Some(f) = free_id(&list, face) else {
        return list;
    };
    let at = match list.last() {
        Some(last) if last.t == Template::Settings => list.len() - 1,
        _ => list.len(),
    };
    let entry = FaceEntry {
        f,
        t: face,
        n: face_label(face).to_string(),
    };
    list.insert(at, entry);
    list
}

/// What sends a strip: the whole list, in order.
pub fn strip_write(list: Vec<FaceEntry>) -> UiAction {
    UiAction::DeviceFacesSet { list }
}

/// Whether two strips are the same faces in the same order. Names are not
/// compared: the deck may shorten one it was given.
pub fn same_strip(a: &[FaceEntry], b: &[FaceEntry]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.f == b.f && a.t == b.t)
}

/// The strip the page shows: the linked deck's own report while it has
/// made one, else the strip the service keeps for the deck.
pub fn strip(mirror: &Snapshot, linked: bool) -> &[FaceEntry] {
    if linked && !mirror.device.faces.is_empty() {
        &mirror.device.faces
    } else {
        &mirror.faces
    }
}

/// The face the page has selected: the one picked while it is on the
/// strip, else the strip's first Macropad (its keys are what the page is
/// most often opened for), else its first face.
pub fn selection(list: &[FaceEntry], chosen: Option<u8>) -> Option<usize> {
    chosen
        .and_then(|f| list.iter().position(|face| face.f == f))
        .or_else(|| list.iter().position(|face| face.t == Template::Macropad))
        .or_else(|| (!list.is_empty()).then_some(0))
}

/// A face's own name in English, the name a face the deck adds is given.
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

/// A strip sent and not yet reported by the deck, and when it was sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub list: Vec<FaceEntry>,
    pub at: Ts,
}

impl Pending {
    /// Whether the deck now reports it.
    pub fn taken(&self, shown: &[FaceEntry]) -> bool {
        same_strip(&self.list, shown)
    }

    /// Whether the deck has had its time to report it.
    pub fn expired(&self, now: Ts) -> bool {
        u128::from(now.millis().saturating_sub(self.at.millis())) >= PENDING_HOLD.as_millis()
    }
}

/// A removed Macropad's keys, to be written empty once the deck reports a
/// strip without it. The service keeps a face's keys whatever the strip
/// says: it would send them to the deck again on the next link, and a
/// Macropad added later in the same place (it gets the same id while that is
/// free, [`free_id`]) would start with them. They are cleared only after the
/// deck took the removal, never before: a deck that refused it (its face
/// manager was open) keeps the face, and the face keeps its keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Forget {
    /// The face that was removed.
    pub f: u8,
    /// `device.macro.set` with no keys and no tag, under this app's layout.
    pub write: UiAction,
}

impl Forget {
    /// What removing `face` leaves to do: for a Macropad the service keeps
    /// keys for, clearing them. `None` for any other face, and for a
    /// Macropad with no keys.
    pub fn of(face: &FaceEntry, mirror: &Snapshot) -> Option<Forget> {
        if face.t != Template::Macropad {
            return None;
        }
        let layout = mirror.macros.get(&face.f)?;
        if crate::keys::trimmed(layout.keys.clone()).is_empty() {
            return None;
        }
        Some(Forget {
            f: face.f,
            write: crate::keys::keys_write(face.f, None, Vec::new()),
        })
    }

    /// Whether the keys can go now: the strip the deck reports `shown` no
    /// longer has the face (a later write could have put it back).
    pub fn due(&self, shown: &[FaceEntry]) -> bool {
        !shown.iter().any(|face| face.f == self.f)
    }
}

/// What the page waits on after it changed the strip, and the deck that was
/// for. Each deck keeps its own strip and keys, so when the service says
/// another deck is in the hand (`device.deck`), what was sent for the last
/// one is let go: a strip it never reported is not waited for, and the keys
/// of a Macropad taken off it are never cleared on this one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StripWrites {
    /// The deck the service keeps the strip and keys of, by its label.
    pub deck: Option<String>,
    /// A strip sent and not yet reported by the deck.
    pub pending: Option<Pending>,
    /// What the pending strip's removal leaves to do once it is taken.
    pub forget: Option<Forget>,
    /// The last strip sent was not taken.
    pub refused: bool,
}

impl StripWrites {
    /// The service now keeps the strip and keys of `deck`. Another deck
    /// than before lets go of everything that was waited on; true then.
    pub fn follow(&mut self, deck: Option<&str>) -> bool {
        if self.deck.as_deref() == deck {
            return false;
        }
        *self = StripWrites {
            deck: deck.map(str::to_string),
            ..StripWrites::default()
        };
        true
    }
}

/// The sentence a strip rule the service refused a strip by comes to, in
/// `language`: `why` is `strip_problem`'s word for it (`device.faces.set`'s
/// `invalid{field:"list", why}`); `None` for words that name no strip rule.
#[must_use]
pub fn strip_rule(why: &str, language: Language) -> Option<&'static str> {
    Some(match why {
        "max 8" => language.text("The strip holds eight faces at most."),
        "f 1..=8" => language.text("Each face needs a number from 1 to 8."),
        "duplicate f" => language.text("Two faces have the same number."),
        "empty name" => language.text("Every face needs a name."),
        "agents first" => language.text("Agents must be the first face."),
        "settings last" => language.text("Settings must be the last face."),
        "agents or settings in the middle" => {
            language.text("Agents and Settings cannot be in the middle.")
        }
        "max 3 macropads" => language.text("The strip can hold three Macropads at most."),
        "max 1 timer" => language.text("The strip can hold one Timer."),
        _ => return None,
    })
}

/// What the toast says when the service would not take a strip: a strip
/// rule as its sentence, in `language`; any other failure in the service's
/// own words (`data.why`), else the error itself.
#[must_use]
pub fn strip_write_failed(error: &ClientError, language: Language) -> String {
    let why = crate::updates::failure_text(error);
    let why = strip_rule(&why, language).map_or(why, str::to_string);
    language.format(
        "{0} failed: {1}",
        &[language.text("Change the deck's faces"), &why],
    )
}

/// What the line beside the strip's buttons says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StripNote {
    /// The strip's limits.
    Limits,
    /// The deck is away: nothing on the strip can change.
    NotLinked,
    /// A strip was sent and the deck has not reported it yet.
    Waiting,
    /// The deck did not take the last one.
    Refused,
}

impl StripNote {
    /// The line for the deck's link and the last write.
    pub fn of(linked: bool, pending: bool, refused: bool) -> StripNote {
        if pending {
            StripNote::Waiting
        } else if !linked {
            StripNote::NotLinked
        } else if refused {
            StripNote::Refused
        } else {
            StripNote::Limits
        }
    }

    /// Its words.
    pub fn words(self, language: Language) -> &'static str {
        match self {
            StripNote::Limits => {
                language.text("Up to eight faces. Agents and Settings stay at the ends.")
            }
            StripNote::NotLinked => language.text("Link the deck to change its faces."),
            StripNote::Waiting => language.text("Waiting for the deck\u{2026}"),
            StripNote::Refused => language
                .text("The deck did not take the change. Close its face manager and try again."),
        }
    }
}

/// What the page shows under the strip for a face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    /// The Macropad's keys.
    Keys,
    /// Where the Usage face's bars come from.
    Usage,
    /// The Agents face's demo sessions.
    Demo,
    /// The Arcade face's sound.
    ArcadeSound,
    /// A face with no settings on the Mac.
    Nothing,
}

impl Panel {
    pub const fn of(face: Template) -> Panel {
        match face {
            Template::Macropad => Panel::Keys,
            Template::Usage => Panel::Usage,
            Template::Agents => Panel::Demo,
            Template::Arcade => Panel::ArcadeSound,
            Template::System | Template::Timer | Template::Settings | Template::Omarchy => {
                Panel::Nothing
            }
        }
    }
}

// ---- the Usage face's sources -------------------------------------------------

/// `usage.providers` as `config.get` publishes it, and whether this service
/// lets the page write it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageConfig {
    pub providers: Vec<String>,
    /// `usage.providers` is a key `config.set` writes. It became one in the
    /// same change that added `api.tcp`, and `config.get` names neither the
    /// keys it writes nor a version, so that key is the mark: a service
    /// that does not publish it refuses the write.
    pub writable: bool,
}

impl UsageConfig {
    pub fn parse(result: &Value) -> UsageConfig {
        let providers = result
            .get("usage")
            .and_then(|usage| usage.get("providers"))
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|name| name.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let writable = result
            .get("api")
            .and_then(|api| api.get("tcp"))
            .is_some_and(Value::is_boolean);
        UsageConfig {
            providers,
            writable,
        }
    }

    /// Whether `provider`'s bars are drawn: the list names it, or a variant
    /// of it (`claude-statusline`), in any case, as the service reads it.
    pub fn on(&self, provider: &str) -> bool {
        self.providers.iter().any(|name| names(name, provider))
    }

    /// The list with `provider` switched on or off. The other names stay as
    /// they are and where they are; switched off, every variant of it goes.
    pub fn switched(&self, provider: &str, on: bool) -> Vec<String> {
        let mut providers: Vec<String> = self
            .providers
            .iter()
            .filter(|name| !names(name, provider))
            .cloned()
            .collect();
        if on {
            providers.push(provider.to_string());
        }
        providers
    }
}

/// Whether `name` in `usage.providers` names `provider`, as the service
/// matches it.
fn names(name: &str, provider: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    name == provider || name.starts_with(&format!("{provider}-"))
}

/// What writes the Usage face's sources: the whole list.
pub fn usage_write(providers: Vec<String>) -> UiAction {
    UiAction::ConfigPatch {
        entries: vec![(PROVIDERS.to_string(), ConfigValue::List(providers))],
    }
}

/// Whether the Usage panel says when its sources are asked: only for a
/// service that asks that way (`service.dormancy`), so the note never
/// claims what an older service does not do.
pub fn usage_asked_note(service: &ServiceState) -> bool {
    service.dormancy.is_some()
}

/// A reply the page keeps: not read yet, a method this service does not
/// have, or the answer.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Reply<T> {
    Unknown,
    Missing,
    Known(T),
}

// ---- the pane -------------------------------------------------------------------

/// The page's own state, which the shell owns.
pub struct FacesPane {
    /// The face picked on the strip, by its id.
    selected: Option<u8>,
    /// The strip sent and what it waits on, for the deck in the hand.
    writes: StripWrites,
    /// `config.get`'s `usage.providers`.
    usage: Reply<UsageConfig>,
    /// A `config.get` is on its way.
    asking: bool,
    /// The Macropad's key editor.
    pub(crate) keys: KeysPane,
}

impl FacesPane {
    /// The pane, and the subscriptions its key editor needs kept alive.
    pub(crate) fn new(
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) -> (FacesPane, Vec<Subscription>) {
        let (keys, subscriptions) = KeysPane::new(window, cx);
        let pane = FacesPane {
            selected: None,
            writes: StripWrites::default(),
            usage: Reply::Unknown,
            asking: false,
            keys,
        };
        (pane, subscriptions)
    }
}

impl Shell {
    /// Ask the service for the Usage face's sources and for the grant the
    /// deck's shortcuts need, unless the page has them; `force` asks anyway
    /// (a reconnect). The strip and the keys are the mirror's, and the Label
    /// field shows the selected key's label as the page opens (words typed
    /// for that key are kept).
    pub(crate) fn refresh_faces(
        &mut self,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if force || self.faces.usage == Reply::Unknown {
            self.read_usage(window, cx);
        }
        self.refresh_keys(force, window, cx);
        self.sync_label(window, cx);
    }

    /// The page: its head, the strip, and the selected face's panel.
    /// Dimmed, with every control off, while the service is not answering.
    pub(crate) fn render_faces(&self, dim: bool, cx: &Context<Self>) -> AnyElement {
        let p = *palette(cx);
        let header = ui::Header::new(
            self.tr(Screen::FacesKeys.label()),
            self.tr(
                "The strip you slide through with SHIFT and the wheel, and what each macro key \
                 types.",
            ),
        );
        let body = v_flex()
            .gap(px(GAP))
            .child(header)
            .child(self.face_strip(dim, &p, cx))
            .children(self.face_panel(dim, &p, cx))
            .when(dim, |body| body.opacity(DIM));
        ui::page("faces", &self.page_scroll, self.content_width, body).into_any_element()
    }

    /// The page's dialog `id` and its width: the faces that can be added.
    pub(crate) fn faces_dialog(
        &self,
        id: &'static str,
        cx: &Context<Self>,
    ) -> Option<(AnyElement, f32)> {
        (id == ADD_FACE).then(|| self.render_add_face(cx))
    }

    /// The add dialog keeps nothing of its own to let go of.
    pub(crate) fn faces_dialog_closed(&mut self, _id: &'static str, _cx: &mut Context<Self>) {}

    /// Nothing the page says lives outside its render.
    pub(crate) fn localize_faces(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    /// A strip the deck now reports is no longer waited for, and a Macropad
    /// it took off goes with its keys; the key editor follows the mirror.
    /// Another deck in the hand lets go of all of it first, and of what the
    /// key editor held for the last one: the Label field is emptied and
    /// given this deck's label at once, whatever page is open, so the last
    /// deck's words are neither shown nor committed to this one.
    pub(crate) fn on_faces_event(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let deck = self.state.mirror().device.deck.clone();
        if self.faces.writes.follow(deck.as_deref()) {
            self.faces.keys.other_deck();
            self.faces.keys.clear_label(window, cx);
            self.sync_label(window, cx);
        }
        let taken = self
            .faces
            .writes
            .pending
            .as_ref()
            .is_some_and(|pending| pending.taken(self.strip_shown()));
        if taken {
            self.faces.writes.pending = None;
            if let Some(forget) = self.faces.writes.forget.take() {
                if forget.due(self.strip_shown()) {
                    self.act(forget.write, window, cx);
                }
            }
        }
        self.on_keys_event(window, cx);
    }

    /// A strip the deck did not report in time was not taken, and a face it
    /// would have removed keeps its keys; the shell redraws after the tick.
    pub(crate) fn tick_faces(&mut self, _beat: u64, window: &mut Window, cx: &mut Context<Self>) {
        let expired = self
            .faces
            .writes
            .pending
            .as_ref()
            .is_some_and(|pending| pending.expired(present::now()));
        if expired {
            self.faces.writes.pending = None;
            self.faces.writes.forget = None;
            self.faces.writes.refused = true;
        }
        self.tick_keys(window, cx);
    }

    /// The page's scenes. The mirror is the scene's fixture; the page
    /// selects the scene's face (and the fifth key), reads the Usage face's
    /// sources from the config fixture and what the service says of
    /// Accessibility from a reply built here, and opens the add dialog or
    /// the question before a removal for the scenes that show them.
    pub(crate) fn stage_faces(
        &mut self,
        scene: Scene,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.faces.writes.pending = None;
        self.faces.writes.forget = None;
        self.faces.writes.refused = false;
        self.stop_recording();
        let Some((_, face, access)) = SCENES.into_iter().find(|(s, ..)| *s == scene) else {
            return false;
        };
        self.faces.selected = self
            .strip_shown()
            .iter()
            .find(|entry| entry.t == face)
            .map(|entry| entry.f);
        let config: Value = serde_json::from_str(CONFIG_FIXTURE).unwrap_or_default();
        self.faces.usage = Reply::Known(UsageConfig::parse(&config));
        self.faces.keys.reset();
        self.stage_keys(STAGED_KEY, &perm_status(access), window, cx);
        self.go(Screen::FacesKeys, window, cx);
        match scene {
            Scene::FacesAdd => self.open_add_face(window, cx),
            Scene::FacesRemove => self.ask_remove(window, cx),
            _ => {}
        }
        true
    }

    /// The strip the page shows.
    fn strip_shown(&self) -> &[FaceEntry] {
        strip(self.state.mirror(), self.linked())
    }

    /// The face selected on the strip.
    pub(crate) fn selected_face(&self) -> Option<FaceEntry> {
        let list = self.strip_shown();
        selection(list, self.faces.selected).map(|at| list[at].clone())
    }

    // ---- what a press does ---------------------------------------------------

    /// Select face `f`. What was typed for a key goes first.
    fn select_face(&mut self, f: u8, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_face().is_some_and(|face| face.f == f) {
            return;
        }
        self.commit_label(window, cx);
        self.faces.keys.reset();
        self.faces.selected = Some(f);
        self.faces.writes.refused = false;
        self.sync_label(window, cx);
        cx.notify();
    }

    /// Send a new strip, and wait for the deck to report it.
    fn write_strip(&mut self, list: Vec<FaceEntry>, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_recording();
        self.faces.writes.refused = false;
        self.faces.writes.pending = Some(Pending {
            list: list.clone(),
            at: present::now(),
        });
        self.ask(strip_write(list), window, cx, |this, result, window, cx| {
            if let Err(e) = result {
                this.faces.writes.pending = None;
                this.faces.writes.forget = None;
                this.toast(strip_write_failed(&e, this.language()), window, cx);
            }
        });
        cx.notify();
    }

    /// **Move Left** and **Move Right**: the selected face changes places
    /// with its neighbour, and stays selected.
    fn move_face(&mut self, side: Side, window: &mut Window, cx: &mut Context<Self>) {
        let list = self.strip_shown().to_vec();
        let Some(at) = selection(&list, self.faces.selected) else {
            return;
        };
        let new = moved(&list, at, side);
        if same_strip(&new, &list) {
            return;
        }
        self.faces.selected = Some(list[at].f);
        self.write_strip(new, window, cx);
    }

    /// **Remove from Deck…**: asks first, saying what goes with the face.
    fn ask_remove(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let list = self.strip_shown();
        let Some(at) = selection(list, self.faces.selected).filter(|&at| can_remove(list, at))
        else {
            return;
        };
        let face = list[at].clone();
        let name = face_name(&face, self.language());
        let body = if face.t == Template::Macropad {
            self.tr("Its keys are removed with it.")
        } else {
            self.tr("You can add it again later.")
        };
        let sheet = Sheet {
            title: self.fmt("Remove {0} from the deck?", &[&name]),
            body: body.to_string(),
            confirm: self.tr("Remove").to_string(),
            danger: true,
        };
        self.ask_first(sheet, window, cx, move |this, window, cx| {
            this.remove_face(face.f, window, cx);
        });
    }

    /// Take face `f` off the strip; the face before it is selected. A
    /// Macropad's keys are cleared once the deck has taken it off.
    fn remove_face(&mut self, f: u8, window: &mut Window, cx: &mut Context<Self>) {
        let list = self.strip_shown().to_vec();
        let Some(at) = list.iter().position(|face| face.f == f) else {
            return;
        };
        if !can_remove(&list, at) {
            return;
        }
        self.faces.selected = at.checked_sub(1).map(|before| list[before].f);
        self.faces.keys.reset();
        // Before the write: a write refused at once lets it go again.
        self.faces.writes.forget = Forget::of(&list[at], self.state.mirror());
        self.write_strip(removed(&list, at), window, cx);
    }

    /// **Add a face**: the dialog of the faces the strip can take.
    fn open_add_face(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_recording();
        self.open_dialog(ADD_FACE, window);
        cx.notify();
    }

    /// Add `face` before Settings; it is selected once the deck has it.
    fn add_face(&mut self, face: Template, window: &mut Window, cx: &mut Context<Self>) {
        self.close_dialog(window, cx);
        let list = self.strip_shown().to_vec();
        if !addable(&list, LINUX).contains(&face) {
            return;
        }
        self.faces.selected = free_id(&list, face);
        self.faces.keys.reset();
        self.write_strip(added(&list, face), window, cx);
    }

    /// Read the Usage face's sources, unless they are being read already.
    fn read_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.faces.asking {
            return;
        }
        self.faces.asking = true;
        self.ask(UiAction::ConfigGet, window, cx, |this, result, _, _| {
            this.faces.asking = false;
            match result {
                Ok(result) => this.faces.usage = Reply::Known(UsageConfig::parse(&result)),
                Err(e) if method_missing(&e) => this.faces.usage = Reply::Missing,
                // Not answered: what was known stays, and the next visit or
                // reconnection asks again.
                Err(_) => {}
            }
        });
    }

    /// A Usage switch: the list written whole, shown at once, and read back.
    fn switch_usage(
        &mut self,
        provider: &'static str,
        on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Reply::Known(config) = &self.faces.usage else {
            return;
        };
        let providers = config.switched(provider, on);
        self.faces.usage = Reply::Known(UsageConfig {
            providers: providers.clone(),
            writable: config.writable,
        });
        self.ask(
            usage_write(providers),
            window,
            cx,
            |this, result, window, cx| {
                if let Err(e) = result {
                    let what = this.tr("Change the config");
                    this.toast(
                        this.fmt("{0} failed: {1}", &[what, &e.to_string()]),
                        window,
                        cx,
                    );
                }
                this.read_usage(window, cx);
            },
        );
        cx.notify();
    }

    // ---- the strip ---------------------------------------------------------

    /// The strip: a tile for each face, **Add a face** while one more fits,
    /// the buttons that change it and the line that says what they can do.
    ///
    /// The tiles are one stop on the Tab order ([`ui::Group`]): Tab reaches
    /// the selected face, Left and Right move the keyboard along the tiles,
    /// and Return or Space presses the one that has it (selects a face,
    /// opens **Add a face**). Option-Left and Option-Right move the
    /// selected face, as **Move Left** and **Move Right** do.
    fn face_strip(&self, dim: bool, p: &Palette, cx: &Context<Self>) -> Div {
        let linked = self.linked();
        let list = self.strip_shown();
        let at = selection(list, self.faces.selected);
        let pending = self.faces.writes.pending.is_some();
        let writable = linked && !pending && !dim;
        let add = !addable(list, LINUX).is_empty();
        let full = list.len() + usize::from(add) >= FULL_COLUMNS;
        let mut tiles = ui::Group::new("face-strip", Layout::Row)
            .h_flex()
            .w_full()
            .items_start()
            .map(|row| {
                if full {
                    row.gap(px(FULL_GAP)).justify_between()
                } else {
                    row.gap(px(TILE_GAP))
                }
            })
            .pt(px(16.))
            .px(px(16.))
            .pb(px(TILES_BOTTOM))
            .selected(at);
        for (i, face) in list.iter().enumerate() {
            let (key, tile) = self.face_tile(face, Some(i) == at, p, cx);
            tiles = tiles.wrapped(key, true, tile);
        }
        if add {
            let (key, tile) = self.add_tile(writable, p, cx);
            tiles = tiles.wrapped(key, writable, tile);
        }
        let allowed =
            |rule: fn(&[FaceEntry], usize) -> bool| writable && at.is_some_and(|at| rule(list, at));
        // With the service away the frame says so; the line keeps to the
        // strip and does not blame the link.
        let note = StripNote::of(linked || dim, pending, self.faces.writes.refused);
        let buttons = h_flex()
            .w_full()
            .gap(px(8.))
            .px(px(16.))
            .py(px(10.))
            .child(
                Btn::new("face-left", self.tr("Move Left"))
                    .quiet()
                    .disabled(!allowed(can_move_left))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.move_face(Side::Left, window, cx);
                    })),
            )
            .child(
                Btn::new("face-right", self.tr("Move Right"))
                    .quiet()
                    .disabled(!allowed(can_move_right))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.move_face(Side::Right, window, cx);
                    })),
            )
            .child(
                Btn::new("face-remove", self.tr("Remove from Deck\u{2026}"))
                    .quiet()
                    .disabled(!allowed(can_remove))
                    .on_click(cx.listener(|this, _, window, cx| this.ask_remove(window, cx))),
            )
            .child(self.strip_note(note, p));
        ui::section(
            self.tr("Face strip"),
            ui::card(
                vec![tiles.into_any_element(), buttons.into_any_element()],
                p,
            ),
            p,
        )
        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
            let stroke = &event.keystroke;
            let Some(side) = strip_move(&stroke.key, &stroke.modifiers) else {
                return;
            };
            cx.stop_propagation();
            // As the Move buttons: only with the deck linked and no strip
            // still on its way to it.
            if this.linked() && this.faces.writes.pending.is_none() {
                this.move_face(side, window, cx);
            }
        }))
    }

    /// One face on the strip: its plate, ringed while selected, and its name
    /// under it with a pin when it stays where it is. A press on either
    /// selects it. The column takes the name and pin's width when that is
    /// more than the plate's, up to [`COLUMN_MAX`]; a longer name is cut.
    ///
    /// The plate is the strip's member: this answers its id and what draws
    /// the tile once the strip has given the plate its keyboard.
    fn face_tile(
        &self,
        face: &FaceEntry,
        selected: bool,
        p: &Palette,
        cx: &Context<Self>,
    ) -> (ElementId, impl FnOnce(&Roving) -> AnyElement + 'static) {
        let f = face.f;
        let id = ElementId::from(("face", usize::from(f)));
        let plate = Press::new(id.clone())
            .radius(TILE_RADIUS)
            .rest(p.tile_rest)
            .selected(selected)
            .flex_none()
            .size(px(TILE))
            .child(art::plate(Some(face.t), TILE, Some(TILE_RADIUS), None))
            .on_click(cx.listener(move |this, _, window, cx| this.select_face(f, window, cx)));
        let name = h_flex()
            .gap(px(4.))
            .ts(13., 18.)
            .map(|name| {
                if selected {
                    name.semibold().text_color(p.text)
                } else {
                    name.medium().text_color(p.text2)
                }
            })
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .child(face_name(face, self.language())),
            )
            .when(pinned(face.t), |name| {
                name.child(ui::icon(ic::PIN, 11., p.text3))
            });
        let column = v_flex()
            .id(("face-tile", usize::from(f)))
            .flex_none()
            .min_w(px(TILE))
            .max_w(px(COLUMN_MAX))
            .gap(px(8.))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| this.select_face(f, window, cx)));
        let draw = move |roving: &Roving| {
            column
                .child(plate.roving(roving))
                .child(name)
                .into_any_element()
        };
        (id, draw)
    }

    /// **Add a face**: a dashed tile with a plus, faint while the strip
    /// cannot change. Like a face's plate, the tile is the strip's member:
    /// this answers its id and what draws it with its keyboard.
    fn add_tile(
        &self,
        enabled: bool,
        p: &Palette,
        cx: &Context<Self>,
    ) -> (ElementId, impl FnOnce(&Roving) -> AnyElement + 'static) {
        let id = ElementId::from("face-add");
        let tile = Press::new(id.clone())
            .radius(TILE_RADIUS)
            .own_border(1.5)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(TILE))
            .rounded(px(TILE_RADIUS))
            .border(px(1.5))
            .border_dashed()
            .border_color(p.edge)
            .child(ui::icon(ic::PLUS, 18., p.text2));
        let tile = if enabled {
            tile.on_click(cx.listener(|this, _, window, cx| this.open_add_face(window, cx)))
        } else {
            tile
        };
        let column = v_flex()
            .flex_none()
            .w(px(TILE))
            .gap(px(8.))
            .when(!enabled, |column| column.opacity(0.4));
        let name = div()
            .ts(13., 18.)
            .medium()
            .whitespace_nowrap()
            .text_color(p.text2)
            .child(self.tr("Add a face"));
        let draw = move |roving: &Roving| {
            column
                .child(tile.roving(roving))
                .child(name)
                .into_any_element()
        };
        (id, draw)
    }

    /// The line at the end of the buttons: the strip's limits, why it cannot
    /// change, the wait for the deck, or that the deck did not take it.
    fn strip_note(&self, note: StripNote, p: &Palette) -> Div {
        let words = note.words(self.language());
        h_flex()
            .flex_1()
            .min_w_0()
            .justify_end()
            .gap(px(6.))
            .ts(12., 16.)
            .text_color(if note == StripNote::Refused {
                p.warn_text
            } else {
                p.text2
            })
            .when(note == StripNote::Waiting, |line| {
                line.child(ui::spinner(12., p.text2))
            })
            .child(div().min_w_0().child(ui::para(words)))
    }

    // ---- under the strip ---------------------------------------------------

    /// What the selected face does on the Mac.
    fn face_panel(&self, dim: bool, p: &Palette, cx: &Context<Self>) -> Option<AnyElement> {
        let face = self.selected_face()?;
        Some(match Panel::of(face.t) {
            Panel::Keys => self.render_keys(dim, p, cx).into_any_element(),
            Panel::Usage => self.usage_panel(dim, p, cx).into_any_element(),
            Panel::Demo | Panel::ArcadeSound | Panel::Nothing => {
                self.face_settings(&face, dim, p, cx).into_any_element()
            }
        })
    }

    /// The Usage face: its two sources with their switches, and its plate.
    /// A service that cannot write the list shows the sources without
    /// switches, and says so.
    fn usage_panel(&self, dim: bool, p: &Palette, cx: &Context<Self>) -> Div {
        let switch = |provider: &'static str, config: &UsageConfig| {
            let id = if provider == CLAUDE {
                "usage-claude"
            } else {
                "usage-codex"
            };
            Switch::new(id, config.on(provider))
                .disabled(dim)
                .on_toggle(cx.listener(move |this, on: &bool, window, cx| {
                    this.switch_usage(provider, *on, window, cx);
                }))
        };
        let claude = Row::new()
            .lead(ui::tile(ic::CLAUDE, p))
            .title(self.tr("Claude usage"))
            .sub(self.tr(
                "Reads your Claude Code login from the Keychain to ask\nAnthropic for your usage. \
                 macOS asks once.",
            ));
        let codex = Row::new()
            .lead(ui::tile(ic::OPENAI, p))
            .title(self.tr("Codex usage"))
            .sub(self.tr("From Codex. The session and weekly windows."));
        let (claude, codex, older) = match &self.faces.usage {
            Reply::Known(config) if config.writable => (
                claude.right(switch(CLAUDE, config)),
                codex.right(switch(CODEX, config)),
                false,
            ),
            Reply::Known(_) | Reply::Missing => (claude, codex, true),
            Reply::Unknown => (claude, codex, false),
        };
        // The design's row is 65 high with its rule; the kit's is 64 and
        // the card draws its rule over the row's last pixel.
        let mut rows = vec![
            div().pb(px(1.)).child(claude).into_any_element(),
            codex.into_any_element(),
        ];
        if older {
            rows.push(note(self.tr("This needs a newer background service."), p));
        }
        // Said only by a service that does it (`service.dormancy`): one that
        // predates the quiet asks whether or not a deck is there.
        if usage_asked_note(&self.state.mirror().service) {
            rows.push(note(
                self.tr("Asked only while this face is on the strip,\n\
                     and for up to ten minutes after the deck was last linked."),
                p,
            ));
        }
        let sources = v_flex()
            .flex_none()
            .w(px(SETTINGS_WIDTH))
            .gap(px(8.))
            .child(ui::label(
                self.tr("Usage face: where the bars come from"),
                p,
            ))
            .child(ui::card(rows, p).flex_1());
        // The two columns are as tall as each other.
        div()
            .w_full()
            .flex()
            .items_stretch()
            .gap(px(GAP))
            .child(sources)
            .child(self.on_the_deck(
                Template::Usage,
                Some(self.tr("One bar for each usage window.")),
                true,
                p,
            ))
    }

    /// Any other face: its few settings (Agents' demo sessions, Arcade's
    /// sound) or a line saying it has none here, beside its plate. The deck
    /// takes a setting only while it is linked.
    fn face_settings(&self, face: &FaceEntry, dim: bool, p: &Palette, cx: &Context<Self>) -> Div {
        let settings = self.state.mirror().settings;
        let settable = self.linked() && !dim;
        let body = match Panel::of(face.t) {
            Panel::Demo => Row::new()
                .title(self.tr("Demo data"))
                .sub(self.tr("Sample sessions on the deck while nothing is connected."))
                .right(
                    Switch::new("face-demo", settings.demo != 0)
                        .disabled(!settable)
                        .on_toggle(cx.listener(|this, on: &bool, window, cx| {
                            let value = u16::from(*on);
                            this.act(
                                UiAction::DeviceSet {
                                    key: SetKey::Demo,
                                    value,
                                },
                                window,
                                cx,
                            );
                        })),
                )
                .into_any_element(),
            Panel::ArcadeSound => Row::new()
                .title(self.tr("Arcade sound"))
                .right(
                    Segmented::new(
                        "face-arcade-sound",
                        vec![
                            self.tr("Off").into(),
                            self.tr("SFX only").into(),
                            self.tr("Music + SFX").into(),
                        ],
                        usize::from(settings.arcadesound.min(2)),
                    )
                    .disabled(!settable)
                    .on_select(cx.listener(|this, at: &usize, window, cx| {
                        let value = u16::try_from(*at).unwrap_or_default();
                        let key = SetKey::Arcadesound;
                        this.act(UiAction::DeviceSet { key, value }, window, cx);
                    })),
                )
                .into_any_element(),
            Panel::Keys | Panel::Usage | Panel::Nothing => {
                note(self.tr("This face has no settings on the Mac."), p)
            }
        };
        let column = v_flex()
            .flex_none()
            .w(px(SETTINGS_WIDTH))
            .gap(px(8.))
            .child(ui::label(face_name(face, self.language()), p))
            .child(ui::card(vec![body], p));
        h_flex()
            .w_full()
            .items_start()
            .gap(px(GAP))
            .child(column)
            .child(self.on_the_deck(face.t, None, false, p))
    }

    /// The face's plate as the deck draws it, titled, on the deck's stage;
    /// `fill` stretches the stage to its column.
    fn on_the_deck(&self, face: Template, caption: Option<&str>, fill: bool, p: &Palette) -> Div {
        let title: SharedString = self.tr(art::plate_title(face)).to_string().into();
        let stage = v_flex()
            .items_center()
            .justify_center()
            .gap(px(12.))
            .pt(px(20.))
            .pb(px(14.))
            .rounded(px(12.))
            .bg(p.stage)
            .border_1()
            .border_color(p.line)
            .when(fill, |stage| stage.flex_1())
            .child(art::plate(Some(face), PREVIEW, None, Some(title)))
            .children(caption.map(|caption| {
                div()
                    .ts(12., 16.)
                    .text_color(p.text2)
                    .child(caption.to_string())
            }));
        v_flex()
            .flex_1()
            .min_w_0()
            .gap(px(8.))
            .child(ui::label(self.tr("On the deck"), p))
            .child(stage)
    }

    // ---- the add dialog ----------------------------------------------------

    /// The faces the strip can take, each its plate and its name; a press
    /// adds it. As wide as its row of plates. The plates are one stop on the
    /// Tab order, as the strip's tiles are: the arrows move the keyboard
    /// along them, and Return or Space adds the one that has it.
    fn render_add_face(&self, cx: &Context<Self>) -> (AnyElement, f32) {
        let p = *palette(cx);
        let language = self.language();
        let choices = addable(self.strip_shown(), LINUX);
        let mut plates = ui::Group::new("add-face-plates", Layout::Row)
            .h_flex()
            .items_start()
            .gap(px(TILE_GAP));
        for &face in &choices {
            let id = ElementId::from(("add-face", usize::from(default_id(face))));
            let plate = Press::new(id.clone())
                .radius(TILE_RADIUS)
                .rest(p.tile_rest)
                .flex_none()
                .size(px(TILE))
                .child(art::plate(Some(face), TILE, Some(TILE_RADIUS), None))
                .on_click(cx.listener(move |this, _, window, cx| this.add_face(face, window, cx)));
            let column = v_flex().flex_none().w(px(TILE)).gap(px(8.));
            let name = div()
                .ts(13., 18.)
                .medium()
                .truncate()
                .text_color(p.text2)
                .child(language.text(face_label(face)));
            plates = plates.wrapped(id, true, move |roving: &Roving| {
                column
                    .child(plate.roving(roving))
                    .child(name)
                    .into_any_element()
            });
        }
        let count = choices.len().max(1) as f32;
        let width = (40. + count * TILE + (count - 1.) * TILE_GAP).max(320.);
        let body = v_flex()
            .p(px(20.))
            .gap(px(16.))
            .child(div().ts(13., 18.).semibold().child(self.tr("Add a face")))
            .child(plates)
            .child(
                h_flex()
                    .justify_end()
                    .child(Btn::new("add-face-cancel", self.tr("Cancel")).on_click(
                        cx.listener(|this, _, window, cx| this.close_dialog(window, cx)),
                    )),
            );
        (body.into_any_element(), width)
    }
}

/// A sentence in a card, set as the design's notes are.
fn note(text: &str, p: &Palette) -> AnyElement {
    div()
        .px(px(16.))
        .py(px(12.))
        .ts(12., 16.)
        .text_color(p.text2)
        .child(ui::para(text.to_string()))
        .into_any_element()
}

/// A `perm.status` reply as the service gives it, with `accessibility` as
/// a scene has it.
fn perm_status(accessibility: &str) -> Value {
    serde_json::json!({
        "microphone": "unknown",
        "speech": "not_determined",
        "bluetooth": "granted",
        "accessibility": accessibility,
        "limits": [],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Access;
    use crate::shots::{board_snapshot, BOARD_MACROPAD};

    #[test]
    fn the_scenes_are_staged_from_replies_the_page_reads_with_its_own_parsers() {
        let config: Value = serde_json::from_str(CONFIG_FIXTURE).unwrap();
        let usage = UsageConfig::parse(&config);
        assert_eq!(usage.providers, ["codex"]);
        assert!(usage.writable);
        for (scene, face, access) in SCENES {
            let want = if scene == Scene::FacesNoAccess {
                Access::Denied
            } else {
                Access::Granted
            };
            assert_eq!(Access::parse(&perm_status(access)), Some(want), "{scene:?}");
            // The scene's face is on its fixture's strip, and the scene is
            // this page's.
            let fixture = scene.fixture().expect("a staged scene");
            assert!(
                fixture.mirror.faces.iter().any(|entry| entry.t == face),
                "{scene:?}"
            );
            assert_eq!(scene.screen(), Screen::FacesKeys, "{scene:?}");
        }
        // The key the boards select is their screenshot key.
        let keys = &board_snapshot().macros[&BOARD_MACROPAD].keys;
        assert_eq!(keys[STAGED_KEY].label, "screenshot");
    }
}
