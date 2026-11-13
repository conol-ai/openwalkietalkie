//! `openwalkietalkie-app --shots DIR`: open the real window, walk it through
//! scenes, save each as `DIR/<scene>-<dark|light>.png`, and quit.
//!
//! It is how the look is checked without anyone taking a screenshot: the
//! capture is this process's own window ([`crate::snapshot`]), which needs no
//! Screen Recording grant. One appearance per run
//! (`OPENWALKIETALKIE_APPEARANCE`), at the boards' size, 1040×780
//! ([`SIZE_ENV`] draws at another, under the same names), and the standard
//! look whatever this Mac's Increase contrast says, unless
//! `OPENWALKIETALKIE_CONTRAST=more` asks for the contrast palette
//! ([`crate::style::look_of`]); the names do not say which.
//!
//! **Two walks, and never the installed service.**
//!
//! * [`SYNTHETIC_ENV`]`=1` walks the boards and the states the boards do not
//!   draw ([`Walk::Boards`]). No socket is dialled. Each scene is drawn over
//!   its [`Fixture`] — the linked mirror every board shares
//!   ([`board_snapshot`]) or the not-linked one ([`offline_snapshot`]),
//!   whether the service is answering, the colour of the case — and the
//!   page the scene belongs to stages the rest (its own panes) in its
//!   `stage_<p>`. The clock is pinned to the boards' minute, 10:02 on
//!   2026-11-11 on this Mac's clock ([`board_now`]), so "4 s ago" and
//!   "today, 09:41" come out the same in every capture.
//! * `DECKD_DATA_DIR` set walks the live scenes ([`Walk::Live`]) against
//!   the test service on that directory: each opens its page, has it ask
//!   for everything again, waits, and draws what the service said. No
//!   fixture, and this Mac's own clock.
//!
//! A run with neither draws nothing: it would be drawing the owner's own
//! service, and a capture of that is his and not a test's. The walk is
//! every scene its kind draws, or the ones [`ONLY_ENV`]`=main,voice` names.
//!
//! A scene speaks its own language — a board's is English, `deck-zh`'s
//! Simplified Chinese — unless [`LANGUAGE_ENV`] (`en`, `zh-Hans`, `zh-Hant`,
//! `ja`) names one for every scene. Either way only the window's mirror
//! changes, never the deck's setting.
//!
//! **Keys.** [`KEYS_ENV`]`=tab;down;enter` types into the window after each
//! scene's capture, one step at a time, and captures every step as
//! `DIR/<scene>-k<n>-<appearance>.png`: a step is keystrokes in gpui's own
//! spelling (`tab`, `shift-tab`, `alt-right`, `cmd-2`), separated by spaces.
//! Each is a whole press, down and then up, as a person's would be: the
//! down through the window's own focus and key bindings
//! (`Window::dispatch_keystroke`), and for Return and Space the release too,
//! as an AppKit key-up to gpui's own view (`keyUp:`)
//! ([`crate::snapshot::release_key`]), since gpui clicks a focused control
//! when the key is let go. Nothing reaches outside this window: it is how the
//! keyboard's way through a page is seen.
//!
//! **Preflight.** [`PREFLIGHT_ENV`]`=1` draws `main` and `tokens` as
//! `DIR/preflight-<scene>-<appearance>.png` and exits: 0 when the two images
//! differ, [`PREFLIGHT_SAME`] when they are one image — one MD5, so the
//! window was not drawn: a display asleep or locked, a window covered — and
//! [`PREFLIGHT_FAILED`] when a capture failed outright. A caller runs it
//! first and does not trust a walk it did not pass.
//!
//! Every page is told every scene, so a page can put away what it left up
//! for the last one; a dialog a scene leaves up is closed without being
//! answered, and nothing a scene does reaches past the window and the
//! service it is pointed at.
use crate::prefs::Case;
use crate::present;
use crate::shell::{Screen, Shell};
use deck_gui_core::i18n::Language;
use deck_gui_core::{ApiEvent, AppEvent};
use deck_model::{
    Binding, DeviceState, DormancyState, FaceEntry, LastLinked, Link, LinkDrop, MacroKey,
    MacroLayout, Permission, Permissions, Route, ServiceState, SessionState, Settings, Snapshot,
    Template, Transport, Ts, VoiceState, VoiceTransport,
};
use gpui::{App, Context, Entity, Global, Window};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// `1`: walk the boards from fixtures, with no service at all.
pub const SYNTHETIC_ENV: &str = "OPENWALKIETALKIE_SHOTS_SYNTHETIC";
/// A comma-separated list of scene names: walk only these.
pub const ONLY_ENV: &str = "OPENWALKIETALKIE_SHOTS_ONLY";
/// `WxH`: draw at this size instead of the boards'.
pub const SIZE_ENV: &str = "OPENWALKIETALKIE_SHOTS_SIZE";
/// A language code: every scene in this language.
pub const LANGUAGE_ENV: &str = "OPENWALKIETALKIE_SHOTS_LANGUAGE";
/// `1`: the preflight, and nothing else.
pub const PREFLIGHT_ENV: &str = "OPENWALKIETALKIE_SHOTS_PREFLIGHT";
/// Steps of keystrokes typed into each scene after its capture, `;` between
/// steps and spaces between keystrokes: `tab;down down;enter`.
pub const KEYS_ENV: &str = "OPENWALKIETALKIE_SHOTS_KEYS";
/// `1`: after each scene's capture, print the menu bar as AppKit holds it
/// (the top menus' titles and the windows menu's) on stderr as
/// `shots-menus: <scene>: …`.
pub const MENUS_ENV: &str = "OPENWALKIETALKIE_SHOTS_MENUS";

/// The preflight's two scenes came out as one image: the window was not
/// drawn.
pub const PREFLIGHT_SAME: i32 = 3;
/// A preflight capture failed outright.
pub const PREFLIGHT_FAILED: i32 = 4;

/// The size every board is drawn at: the window's own, 1040×780.
pub const BOARD_SIZE: (f32, f32) = (1040., 780.);

/// How long a staged scene is given to settle.
const SETTLE: Duration = Duration::from_millis(1600);
/// How long a live scene is given to ask the service and draw its answers.
const LIVE_SETTLE: Duration = Duration::from_secs(4);
/// How long a live walk waits for the test service before its first scene.
const CONNECT_WAIT: Duration = Duration::from_secs(10);
/// How often it looks.
const POLL: Duration = Duration::from_millis(500);

/// Two scenes that draw two different screens whatever the pages are: the
/// preflight's.
const PREFLIGHT: [(Scene, &str); 2] = [(Scene::Main, "main"), (Scene::Tokens, "tokens")];

/// Why a run with neither walk's variable set draws nothing.
const NO_WALK: &str = "a walk draws the boards with OPENWALKIETALKIE_SHOTS_SYNTHETIC=1, or a test \
     service's live scenes with DECKD_DATA_DIR set; without either it would draw the installed \
     service, and it never does";

/// Where the PNGs go.
pub struct Shots {
    dir: PathBuf,
}

impl Global for Shots {}

impl Shots {
    /// [`SYNTHETIC_ENV`]`=1`: no service at all.
    pub fn synthetic_only(&self) -> bool {
        std::env::var(SYNTHETIC_ENV).as_deref() == Ok("1")
    }

    /// This run's walk, from its environment, or why it has none.
    pub fn walk(&self) -> Result<Walk, &'static str> {
        Walk::of(
            self.synthetic_only(),
            std::env::var_os("DECKD_DATA_DIR").is_some_and(|dir| !dir.is_empty()),
        )
    }

    /// Whether the window dials a service: a live walk does, the test
    /// service on `DECKD_DATA_DIR`; any other walk is drawn from fixtures,
    /// and a run with no walk draws nothing.
    pub fn dials(&self) -> bool {
        self.walk() == Ok(Walk::Live)
    }

    /// `--shots DIR` anywhere in the arguments.
    pub fn from_args(mut args: impl Iterator<Item = String>) -> Option<Shots> {
        while let Some(arg) = args.next() {
            if arg == "--shots" {
                return args.next().map(|dir| Shots {
                    dir: PathBuf::from(dir),
                });
            }
        }
        None
    }
}

/// What a walk draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Walk {
    /// The boards and the states they do not draw, from fixtures, with no
    /// service.
    Boards,
    /// The live scenes, from the test service `DECKD_DATA_DIR` names.
    Live,
}

impl Walk {
    /// The walk a run makes: synthetic is the boards (and dials nothing,
    /// whatever else is set), a data directory of its own is live, and with
    /// neither there is no walk.
    pub fn of(synthetic: bool, data_dir: bool) -> Result<Walk, &'static str> {
        if synthetic {
            Ok(Walk::Boards)
        } else if data_dir {
            Ok(Walk::Live)
        } else {
            Err(NO_WALK)
        }
    }

    /// Whether this walk draws `scene`.
    pub fn draws(self, scene: Scene) -> bool {
        scene.live() == (self == Walk::Live)
    }

    /// The scenes this walk takes, in [`SCENES`] order: the ones `only`
    /// names, or every scene it draws. A name it cannot take — a scene of
    /// the other walk, or no scene at all — is left out, and the second list
    /// says why.
    pub fn scenes(self, only: Option<&[String]>) -> (Vec<(Scene, &'static str)>, Vec<String>) {
        let mut refused = Vec::new();
        for name in only.unwrap_or_default() {
            match SCENES.iter().find(|(_, known)| *known == name.as_str()) {
                None => refused.push(format!("{name}: no scene has that name")),
                Some((scene, _)) if !self.draws(*scene) => refused.push(match self {
                    Walk::Boards => format!(
                        "{name}: a live scene draws a test service, and this walk is synthetic; \
                         set DECKD_DATA_DIR instead"
                    ),
                    Walk::Live => format!(
                        "{name}: a board is drawn from its fixture, with \
                         OPENWALKIETALKIE_SHOTS_SYNTHETIC=1, never over a service"
                    ),
                }),
                Some(_) => {}
            }
        }
        let scenes = SCENES
            .into_iter()
            .filter(|(scene, name)| {
                self.draws(*scene) && only.is_none_or(|only| only.iter().any(|o| o == name))
            })
            .collect();
        (scenes, refused)
    }
}

/// One state of the window: a board of the design, a state the design does
/// not draw, or a page as a live service leaves it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scene {
    // The boards.
    Main,
    DeckQuiet,
    DeckZh,
    NotLinked,
    Connect,
    Found,
    FacesKeys,
    FacesUsage,
    Integrations,
    Voice,
    Service,
    Footprint,
    MenuBar,
    Tokens,
    // The states the boards do not draw.
    ServiceDown,
    NotInstalled,
    LinkDetails,
    DeckUnknown,
    FacesFull,
    FacesNoAccess,
    FacesAdd,
    FacesRemove,
    IntegrationsPaused,
    IntegrationsRepair,
    IntegrationsPlan,
    VoiceDownloading,
    VoiceKey,
    ServiceStop,
    ServiceFirmware,
    LocalApi,
    FootprintUninstall,
    FirstVoice,
    FirstIntegrations,
    // The pages as the test service leaves them: no fixture.
    LiveDeck,
    LiveFaces,
    LiveIntegrations,
    LiveVoice,
    LiveService,
    LiveFootprint,
    LiveLocalApi,
}

impl Scene {
    /// The screen the scene shows. First run's steps have none of their own:
    /// first run covers the window, and the Deck is under it.
    pub const fn screen(self) -> Screen {
        match self {
            Scene::Main
            | Scene::DeckQuiet
            | Scene::DeckZh
            | Scene::NotLinked
            | Scene::ServiceDown
            | Scene::NotInstalled
            | Scene::LinkDetails
            | Scene::DeckUnknown
            | Scene::Connect
            | Scene::Found
            | Scene::FirstVoice
            | Scene::FirstIntegrations
            | Scene::LiveDeck => Screen::Deck,
            Scene::FacesKeys
            | Scene::FacesUsage
            | Scene::FacesFull
            | Scene::FacesNoAccess
            | Scene::FacesAdd
            | Scene::FacesRemove
            | Scene::LiveFaces => Screen::FacesKeys,
            Scene::Integrations
            | Scene::IntegrationsPaused
            | Scene::IntegrationsRepair
            | Scene::IntegrationsPlan
            | Scene::LiveIntegrations => Screen::Integrations,
            Scene::Voice | Scene::VoiceDownloading | Scene::VoiceKey | Scene::LiveVoice => {
                Screen::Voice
            }
            Scene::Service | Scene::ServiceStop | Scene::ServiceFirmware | Scene::LiveService => {
                Screen::Service
            }
            Scene::Footprint | Scene::FootprintUninstall | Scene::LiveFootprint => {
                Screen::Footprint
            }
            Scene::LocalApi | Scene::LiveLocalApi => Screen::LocalApi,
            Scene::MenuBar => Screen::MenuPreview,
            Scene::Tokens => Screen::Kit,
        }
    }

    /// A page as the test service leaves it, drawn from no fixture.
    pub const fn live(self) -> bool {
        matches!(
            self,
            Scene::LiveDeck
                | Scene::LiveFaces
                | Scene::LiveIntegrations
                | Scene::LiveVoice
                | Scene::LiveService
                | Scene::LiveFootprint
                | Scene::LiveLocalApi
        )
    }

    /// How long the walk waits before drawing it.
    pub const fn settle(self) -> Duration {
        if self.live() {
            LIVE_SETTLE
        } else {
            SETTLE
        }
    }

    /// What the scene is drawn over; `None` for a live scene, which is drawn
    /// over what the service says.
    pub fn fixture(self) -> Option<Fixture> {
        if self.live() {
            return None;
        }
        let linked = |mirror: Snapshot| Fixture {
            mirror,
            connected: true,
            case: Case::White,
        };
        let fixture = match self {
            // The deck is away; the service says so.
            Scene::NotLinked => linked(offline_snapshot()),
            // The deck is away, and so is the service.
            Scene::ServiceDown => Fixture {
                connected: false,
                ..linked(offline_snapshot())
            },
            // Nothing has been installed: no service has ever answered.
            Scene::NotInstalled | Scene::Connect | Scene::Found => Fixture {
                connected: false,
                ..linked(Snapshot::default())
            },
            // An ordinary day, and a black deck.
            Scene::DeckQuiet => {
                let mut mirror = board_snapshot();
                mirror.device.drops.clear();
                Fixture {
                    case: Case::Black,
                    ..linked(mirror)
                }
            }
            Scene::DeckZh => {
                let mut mirror = board_snapshot();
                mirror.settings.language = Language::SimplifiedChinese as u16;
                mirror.device.settings.language = mirror.settings.language;
                linked(mirror)
            }
            // By cable, and the deck has said neither its charge nor its face.
            Scene::DeckUnknown => {
                let mut mirror = board_snapshot();
                let device = &mut mirror.device;
                device.transport = Some(Transport::Usb);
                device.battery = None;
                device.charging = None;
                device.face = None;
                if let Some(last) = &mut device.last {
                    last.transport = Some(Transport::Usb);
                }
                linked(mirror)
            }
            // The strip full: eight faces, Omarchy among them.
            Scene::FacesFull => {
                let mut mirror = board_snapshot();
                let omarchy = FaceEntry {
                    f: 2,
                    t: Template::Omarchy,
                    n: "Omarchy".into(),
                };
                mirror.faces.insert(1, omarchy);
                mirror.device.faces = mirror.faces.clone();
                linked(mirror)
            }
            _ => linked(board_snapshot()),
        };
        Some(fixture)
    }
}

/// What a board or a state scene is drawn over: the mirror, whether the
/// service is answering, and the colour of the deck's case. The page the
/// scene belongs to stages the rest, its own panes, in `stage_<p>`.
#[derive(Clone, Debug, PartialEq)]
pub struct Fixture {
    pub mirror: Snapshot,
    pub connected: bool,
    pub case: Case,
}

/// Every scene, with the name its file gets.
pub const SCENES: [(Scene, &str); 40] = [
    (Scene::Main, "main"),
    (Scene::DeckQuiet, "deck-quiet"),
    (Scene::DeckZh, "deck-zh"),
    (Scene::NotLinked, "not-linked"),
    (Scene::Connect, "connect"),
    (Scene::Found, "found"),
    (Scene::FacesKeys, "faces-keys"),
    (Scene::FacesUsage, "faces-usage"),
    (Scene::Integrations, "integrations"),
    (Scene::Voice, "voice"),
    (Scene::Service, "service"),
    (Scene::Footprint, "footprint"),
    (Scene::MenuBar, "menubar"),
    (Scene::Tokens, "tokens"),
    (Scene::ServiceDown, "service-down"),
    (Scene::NotInstalled, "not-installed"),
    (Scene::LinkDetails, "link-details"),
    (Scene::DeckUnknown, "deck-unknown"),
    (Scene::FacesFull, "faces-full"),
    (Scene::FacesNoAccess, "faces-no-access"),
    (Scene::FacesAdd, "faces-add"),
    (Scene::FacesRemove, "faces-remove"),
    (Scene::IntegrationsPaused, "integrations-paused"),
    (Scene::IntegrationsRepair, "integrations-repair"),
    (Scene::IntegrationsPlan, "integrations-plan"),
    (Scene::VoiceDownloading, "voice-downloading"),
    (Scene::VoiceKey, "voice-key"),
    (Scene::ServiceStop, "service-stop"),
    (Scene::ServiceFirmware, "service-firmware"),
    (Scene::LocalApi, "local-api"),
    (Scene::FootprintUninstall, "footprint-uninstall"),
    (Scene::FirstVoice, "first-voice"),
    (Scene::FirstIntegrations, "first-integrations"),
    (Scene::LiveDeck, "live-deck"),
    (Scene::LiveFaces, "live-faces"),
    (Scene::LiveIntegrations, "live-integrations"),
    (Scene::LiveVoice, "live-voice"),
    (Scene::LiveService, "live-service"),
    (Scene::LiveFootprint, "live-footprint"),
    (Scene::LiveLocalApi, "live-local-api"),
];

// ---- the boards' deck -----------------------------------------------------

/// The firmware the boards' deck runs, as the deck names it.
pub const BOARD_FW: &str = "0.13.0-openwalkietalkie";
/// The boards' deck's board.
pub const BOARD_REVISION: &str = "Rev B";
/// The Macropad's face on the boards' strip.
pub const BOARD_MACROPAD: u8 = 3;
/// The Main board's reconnects in the last hour.
pub const BOARD_DROPS: u32 = 41;
/// 2026-11-11T00:00:00Z, for a time zone that cannot place the boards' day.
const BOARD_DAY_UTC: u64 = 1_794_355_200_000;

/// `hour:minute:second` on the boards' day, 2026-11-11, on this Mac's clock.
pub fn board_time(hour: u32, minute: u32, second: u32) -> Ts {
    let minute_start = present::local_ts(2026, 11, 11, hour, minute).unwrap_or_else(|| {
        Ts::from_millis(BOARD_DAY_UTC + u64::from(hour * 3_600 + minute * 60) * 1_000)
    });
    minute_start.plus(Duration::from_secs(u64::from(second)))
}

/// The minute every staged scene is drawn at: 10:02 on the boards' day.
pub fn board_now() -> Ts {
    board_time(10, 2, 0)
}

/// The deck every board shows, linked: over Bluetooth at 85 %, a Rev B on
/// 0.13.0, the Agents face up on a strip of seven, the Macropad's eight
/// keys, the settings the Deck board draws (speaker 6, mic 7, announce,
/// chime and auto-jump on, brightness 4, idle home 2 min, medium text,
/// English), and the Main board's 41 reconnects in the hour before
/// [`board_now`]. No session: this window never shows one.
pub fn board_snapshot() -> Snapshot {
    let settings = board_settings();
    Snapshot {
        rev: 1,
        device: DeviceState {
            language_supported: true,
            link: Link::Connected,
            session: SessionState::Live,
            sid: 1,
            fw: Some(BOARD_FW.into()),
            boot: Some(false),
            face: Some(1),
            faces: board_strip(),
            host_online: true,
            settings,
            sessions: Some(8),
            transport: Some(Transport::Bluetooth),
            battery: Some(85),
            charging: Some(false),
            board: Some(BOARD_REVISION.into()),
            // Linked again a moment after the last reconnect.
            last: Some(LastLinked {
                at: board_time(9, 59, 50),
                fw: Some(BOARD_FW.into()),
                board: Some(BOARD_REVISION.into()),
                transport: Some(Transport::Bluetooth),
            }),
            drops: board_drops(),
            ..DeviceState::default()
        },
        voice: VoiceState {
            transport: Some(VoiceTransport::Ble),
            route: Route::Ble,
            // Bluetooth granted, speech recognition not asked yet, and no
            // microphone of this Mac's in the path.
            permissions: Permissions {
                microphone: Permission::Unknown,
                speech: Permission::NotDetermined,
                bluetooth: Permission::Granted,
            },
            ..VoiceState::default()
        },
        settings,
        faces: board_strip(),
        macros: BTreeMap::from([(
            BOARD_MACROPAD,
            MacroLayout {
                src: None,
                keys: board_keys(),
            },
        )]),
        service: ServiceState {
            version: "0.13.0".into(),
            port: 47_201,
            // A service with a deck collects while one was linked in the
            // last ten minutes (`service.dormancy`); this one is linked.
            dormancy: Some(DormancyState {
                window_s: 600,
                collecting: true,
                dormant_since: None,
            }),
            ..ServiceState::default()
        },
        ..Snapshot::default()
    }
}

/// The same deck away: the link dropped at 09:41 on the boards' day and
/// nothing has linked since. What the deck said about itself while linked
/// is gone, and what the service keeps is not — when it was last linked
/// and what it ran, the strip, the keys and the settings.
pub fn offline_snapshot() -> Snapshot {
    let mut snapshot = board_snapshot();
    let gone = board_time(9, 41, 0);
    let device = &mut snapshot.device;
    device.link = Link::Offline;
    device.session = SessionState::Absent;
    device.host_online = false;
    device.language_supported = false;
    device.fw = None;
    device.face = None;
    device.transport = None;
    device.battery = None;
    device.charging = None;
    device.board = None;
    device.last = Some(LastLinked {
        at: gone,
        fw: Some(BOARD_FW.into()),
        board: Some(BOARD_REVISION.into()),
        transport: Some(Transport::Bluetooth),
    });
    device.drops = vec![LinkDrop {
        at: gone,
        why: "removed".into(),
    }];
    snapshot.voice.route = Route::None;
    // Ten minutes after the drop the service stopped collecting: 09:51.
    snapshot.service.dormancy = Some(DormancyState {
        window_s: 600,
        collecting: false,
        dormant_since: Some(gone.plus(Duration::from_secs(600))),
    });
    snapshot
}

/// The boards' settings, as the deck has them.
fn board_settings() -> Settings {
    Settings {
        speaker: 6,
        mic: 7,
        announce: 1,
        chime: 1,
        autojump: 1,
        brightness: 4,
        idlehome: 120,
        textsize: 1,
        language: Language::English as u16,
        ..Settings::default()
    }
}

/// The boards' strip: Agents and Settings at the ends, each face by its
/// template's own id.
fn board_strip() -> Vec<FaceEntry> {
    [
        (1, Template::Agents, "Agents"),
        (BOARD_MACROPAD, Template::Macropad, "Macropad"),
        (4, Template::Usage, "Usage"),
        (7, Template::System, "System"),
        (8, Template::Timer, "Timer"),
        (6, Template::Arcade, "Arcade"),
        (5, Template::Settings, "Settings"),
    ]
    .into_iter()
    .map(|(f, t, n)| FaceEntry { f, t, n: n.into() })
    .collect()
}

/// The FacesKeys board's eight keys, the ninth slot empty: label, the
/// deck's control that presses it, and the shortcut it types.
fn board_keys() -> Vec<MacroKey> {
    [
        ("build", Some(Binding::Left), "cmd+b"),
        ("test", Some(Binding::Right), "cmd+u"),
        ("deploy", Some(Binding::Enter), "shift+cmd+d"),
        ("git push", Some(Binding::ShiftLeft), "shift+cmd+p"),
        ("screenshot", None, "shift+cmd+4"),
        ("lock", None, "ctrl+cmd+q"),
        ("mute", Some(Binding::ShiftRight), "shift+cmd+m"),
        ("term", Some(Binding::ShiftEnter), "ctrl+grave"),
    ]
    .into_iter()
    .map(|(label, binding, keys)| MacroKey {
        label: label.into(),
        binding,
        keys: Some(keys.into()),
    })
    .collect()
}

/// The Main board's reconnects: about one every minute and a half from
/// 09:03 to 10:00, most of them a deck that stopped answering.
fn board_drops() -> Vec<LinkDrop> {
    const WHY: [&str; 5] = ["misses", "misses", "read", "misses", "write_stall"];
    let first = board_time(9, 3, 0);
    (0..BOARD_DROPS)
        .map(|i| LinkDrop {
            at: first.plus(Duration::from_secs(u64::from(i * 85 + i * 7 % 11))),
            why: WHY[i as usize % WHY.len()].into(),
        })
        .collect()
}

// ---- the walk -------------------------------------------------------------

/// Walk the scenes, saving each, then quit; or the preflight, then exit.
pub fn start(shell: Entity<Shell>, window: &mut Window, cx: &mut App) {
    let shots = cx.global::<Shots>();
    let dir = shots.dir.clone();
    let walk = match shots.walk() {
        Ok(walk) => walk,
        Err(why) => {
            eprintln!("openwalkietalkie-app: --shots: {why}");
            cx.quit();
            return;
        }
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "openwalkietalkie-app: --shots cannot use {}: {e}",
            dir.display()
        );
        cx.quit();
        return;
    }
    let (width, height) = std::env::var(SIZE_ENV)
        .ok()
        .and_then(|size| size_named(&size))
        .unwrap_or(BOARD_SIZE);
    window.resize(gpui::size(gpui::px(width), gpui::px(height)));
    let preflight = std::env::var(PREFLIGHT_ENV).as_deref() == Ok("1");
    let scenes = if preflight {
        PREFLIGHT.to_vec()
    } else {
        let only = only_named(std::env::var(ONLY_ENV).ok().as_deref());
        let (scenes, refused) = walk.scenes(only.as_deref());
        for why in refused {
            eprintln!("openwalkietalkie-app: {why}");
        }
        scenes
    };
    let keys = if preflight {
        Vec::new()
    } else {
        keys_named(std::env::var(KEYS_ENV).ok().as_deref())
    };
    let menus = !preflight && std::env::var(MENUS_ENV).as_deref() == Ok("1");
    window
        .spawn(cx, async move |cx| {
            // A live walk gives the test service its time to answer first.
            // One that never does is drawn as it is: the not-running states
            // are scenes too.
            if walk == Walk::Live && !preflight {
                for _ in 0..CONNECT_WAIT.as_millis() / POLL.as_millis() {
                    let connected = cx
                        .update(|_, cx| shell.read(cx).state.connected())
                        .unwrap_or(true);
                    if connected {
                        break;
                    }
                    cx.background_executor().timer(POLL).await;
                }
            }
            // The first frame.
            cx.background_executor().timer(POLL).await;
            let mut captures = Vec::with_capacity(scenes.len());
            for (scene, name) in scenes {
                cx.update(|window, cx| {
                    shell.update(cx, |this, cx| this.stage(scene, window, cx));
                })
                .ok();
                cx.background_executor().timer(scene.settle()).await;
                let file = if preflight {
                    format!("preflight-{name}")
                } else {
                    name.to_string()
                };
                let saved = cx
                    .update(|window, cx| save(window, cx, &dir, &file))
                    .unwrap_or_else(|e| Err(e.to_string()));
                match &saved {
                    Ok(path) => println!("{}", path.display()),
                    Err(e) => eprintln!("openwalkietalkie-app: {name}: {e}"),
                }
                captures.push(saved);
                // The menu bar in the scene's language, as AppKit holds it.
                #[cfg(target_os = "macos")]
                if menus {
                    let (titles, windows) = crate::macos::menu_bar_titles();
                    eprintln!(
                        "shots-menus: {name}: {} | windows menu: {}",
                        titles.join(" · "),
                        windows.as_deref().unwrap_or("none")
                    );
                }
                #[cfg(not(target_os = "macos"))]
                let _ = menus;
                // The keyboard's way through the scene: each step typed into
                // this window as a person would, then captured.
                for (n, step) in keys.iter().enumerate() {
                    cx.update(|window, cx| {
                        for key in step {
                            match gpui::Keystroke::parse(key) {
                                // A whole press, down then up: gpui clicks
                                // a focused control when Return or Space is
                                // let go, which only a key-up carries.
                                Ok(keystroke) => {
                                    #[cfg(target_os = "macos")]
                                    let released = (!keystroke.modifiers.modified())
                                        .then(|| keystroke.key.clone());
                                    window.dispatch_keystroke(keystroke, cx);
                                    #[cfg(target_os = "macos")]
                                    if let Some(key) = released {
                                        if let Err(e) = crate::snapshot::release_key(window, &key) {
                                            eprintln!("openwalkietalkie-app: {name}: {key}: {e}");
                                        }
                                    }
                                }
                                Err(e) => eprintln!("openwalkietalkie-app: {name}: {e}"),
                            }
                        }
                    })
                    .ok();
                    cx.background_executor().timer(scene.settle()).await;
                    let file = format!("{name}-k{}", n + 1);
                    let saved = cx
                        .update(|window, cx| save(window, cx, &dir, &file))
                        .unwrap_or_else(|e| Err(e.to_string()));
                    match &saved {
                        Ok(path) => println!("{}", path.display()),
                        Err(e) => eprintln!("openwalkietalkie-app: {file}: {e}"),
                    }
                }
            }
            if preflight {
                let images: Vec<Result<Vec<u8>, String>> = captures
                    .iter()
                    .map(|saved| {
                        let path = saved.as_ref().map_err(Clone::clone)?;
                        std::fs::read(path).map_err(|e| e.to_string())
                    })
                    .collect();
                let code = preflight_verdict(&images);
                if code == PREFLIGHT_SAME {
                    eprintln!(
                        "openwalkietalkie-app: preflight: two scenes came out as one image, so the \
                         window was not drawn (a display asleep or locked, or the window covered)"
                    );
                }
                std::process::exit(code);
            }
            cx.update(|_, cx| cx.quit()).ok();
        })
        .detach();
}

/// The window, as `DIR/<name>-<dark|light>.png`.
fn save(window: &Window, cx: &App, dir: &Path, name: &str) -> Result<PathBuf, String> {
    let appearance = if crate::style::palette(cx).dark {
        "dark"
    } else {
        "light"
    };
    let path = dir.join(format!("{name}-{appearance}.png"));
    // The window's own NSView is the only snapshot source there is.
    #[cfg(target_os = "macos")]
    {
        crate::snapshot::save_window_png(window, &path).map(|()| path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (window, path);
        Err("window snapshots need macOS".into())
    }
}

/// The preflight's exit code from its two images: they differ (0), they are
/// one image ([`PREFLIGHT_SAME`]), or there are not two to compare
/// ([`PREFLIGHT_FAILED`]). Two files with the same bytes are what one MD5
/// for both means.
pub fn preflight_verdict(images: &[Result<Vec<u8>, String>]) -> i32 {
    match images {
        [Ok(first), Ok(second)] if first == second => PREFLIGHT_SAME,
        [Ok(_), Ok(_)] => 0,
        _ => PREFLIGHT_FAILED,
    }
}

/// [`ONLY_ENV`]'s names; `None` when it names none.
fn only_named(value: Option<&str>) -> Option<Vec<String>> {
    let names: Vec<String> = value?
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect();
    (!names.is_empty()).then_some(names)
}

/// [`KEYS_ENV`]'s steps, each a list of keystrokes; none when it names none.
fn keys_named(value: Option<&str>) -> Vec<Vec<String>> {
    value
        .unwrap_or_default()
        .split(';')
        .map(|step| {
            step.split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|step| !step.is_empty())
        .collect()
}

/// [`SIZE_ENV`]'s `WxH`.
fn size_named(size: &str) -> Option<(f32, f32)> {
    let (width, height) = size.split_once('x')?;
    let (width, height) = (width.trim().parse::<f32>().ok()?, height.trim().parse::<f32>().ok()?);
    (width > 0. && height > 0.).then_some((width, height))
}

/// The language [`LANGUAGE_ENV`] names by its code.
fn language_named(code: &str) -> Option<Language> {
    Language::ALL
        .into_iter()
        .find(|language| language.code() == code.trim())
}

impl Shell {
    /// Put the window in `scene`. Whatever dialog the last scene left up is
    /// closed without being answered; a staged scene's [`Fixture`] goes in
    /// first and the walk's language ([`LANGUAGE_ENV`]) over it, then every
    /// page hears the scene and the one it belongs to stages it, and a scene
    /// no page stages opens its screen as it is. A live scene opens its
    /// screen and has it ask for everything again.
    fn stage(&mut self, scene: Scene, window: &mut Window, cx: &mut Context<Self>) {
        self.close_dialog(window, cx);
        self.page_scroll
            .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
        match scene.fixture() {
            Some(fixture) => {
                present::set_fixed_now(Some(board_now()));
                self.update_prefs(cx, |prefs| prefs.set_case(fixture.case));
                self.state.handle_event(AppEvent::Api(ApiEvent::Connected {
                    snapshot: fixture.mirror,
                }));
                if !fixture.connected {
                    self.state
                        .handle_event(AppEvent::Api(ApiEvent::Disconnected));
                }
            }
            None => present::set_fixed_now(None),
        }
        // The walk's language over the scene's own, in the window's mirror
        // alone: a mirror is not the deck, and nothing is written. It goes
        // in as a re-read, which says nothing about the connection. It goes
        // in before the pages stage the scene: a page words what it opens
        // there once (a sheet's title, sentence and button), so a language
        // that came later would leave those words in the scene's own.
        let language = std::env::var(LANGUAGE_ENV)
            .ok()
            .as_deref()
            .and_then(language_named);
        if let Some(language) = language.filter(|language| *language != self.language()) {
            let mut snapshot = self.state.mirror().clone();
            snapshot.settings.language = language as u16;
            self.state
                .handle_event(AppEvent::Api(ApiEvent::Snapshot { snapshot }));
        }
        // Every page hears every scene, so a page can put away what it
        // left up for the last one; a page answers `true` for its own.
        let staged = [
            self.stage_deck(scene, window, cx),
            self.stage_faces(scene, window, cx),
            self.stage_integrations(scene, window, cx),
            self.stage_voice(scene, window, cx),
            self.stage_service(scene, window, cx),
            self.stage_firstrun(scene, window, cx),
        ];
        if scene.live() || !staged.contains(&true) {
            self.go(scene.screen(), window, cx);
        }
        if scene.live() {
            self.refresh_screen(true, window, cx);
        }
        self.localize_pages(window, cx);
        crate::app::apply_menu_language(self.language(), cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deck_gui_core::attention::{attention, drops_last_hour, Attention, Extras};
    use deck_gui_core::{IconState, MenuModel, UiAction};
    use deck_model::{Principal, Request, SessionId, Store, StoreConfig};

    fn names(scenes: &[(Scene, &str)]) -> Vec<String> {
        scenes.iter().map(|(_, name)| (*name).to_string()).collect()
    }

    #[test]
    fn shots_is_asked_for_by_a_flag_and_a_directory() {
        let args = |list: &[&str]| list.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert!(Shots::from_args(args(&[]).into_iter()).is_none());
        assert!(Shots::from_args(args(&["--shots"]).into_iter()).is_none());
        let shots = Shots::from_args(args(&["--shots", "/tmp/x"]).into_iter()).unwrap();
        assert_eq!(shots.dir, PathBuf::from("/tmp/x"));
    }

    #[test]
    fn every_scene_is_saved_under_a_name_of_its_own() {
        for (i, (scene, name)) in SCENES.iter().enumerate() {
            assert!(
                !SCENES[i + 1..].iter().any(|(_, other)| other == name),
                "{name} twice"
            );
            assert!(
                !SCENES[i + 1..].iter().any(|(other, _)| other == scene),
                "{scene:?} twice"
            );
        }
    }

    #[test]
    fn the_scenes_are_the_boards_their_states_and_the_live_pages() {
        let all: Vec<&str> = SCENES.iter().map(|(_, name)| *name).collect();
        assert_eq!(
            all,
            [
                // The fourteen boards.
                "main",
                "deck-quiet",
                "deck-zh",
                "not-linked",
                "connect",
                "found",
                "faces-keys",
                "faces-usage",
                "integrations",
                "voice",
                "service",
                "footprint",
                "menubar",
                "tokens",
                // The states the boards do not draw.
                "service-down",
                "not-installed",
                "link-details",
                "deck-unknown",
                "faces-full",
                "faces-no-access",
                "faces-add",
                "faces-remove",
                "integrations-paused",
                "integrations-repair",
                "integrations-plan",
                "voice-downloading",
                "voice-key",
                "service-stop",
                "service-firmware",
                "local-api",
                "footprint-uninstall",
                "first-voice",
                "first-integrations",
                // A test service's pages.
                "live-deck",
                "live-faces",
                "live-integrations",
                "live-voice",
                "live-service",
                "live-footprint",
                "live-local-api",
            ]
        );
        for (scene, name) in SCENES {
            assert_eq!(scene.live(), name.starts_with("live-"), "{name}");
            assert_eq!(
                scene.settle(),
                if scene.live() { LIVE_SETTLE } else { SETTLE }
            );
        }
        assert_eq!(names(&PREFLIGHT), ["main", "tokens"]);
        assert_ne!(PREFLIGHT[0].0.screen(), PREFLIGHT[1].0.screen());
    }

    #[test]
    fn every_scene_opens_the_screen_of_its_board() {
        let screen = |name: &str| {
            SCENES
                .iter()
                .find(|(_, n)| *n == name)
                .map(|(scene, _)| scene.screen())
        };
        for name in [
            "main",
            "deck-zh",
            "not-linked",
            "link-details",
            "connect",
            "found",
            "live-deck",
        ] {
            assert_eq!(screen(name), Some(Screen::Deck), "{name}");
        }
        assert_eq!(screen("faces-usage"), Some(Screen::FacesKeys));
        assert_eq!(screen("live-faces"), Some(Screen::FacesKeys));
        assert_eq!(screen("integrations-plan"), Some(Screen::Integrations));
        assert_eq!(screen("live-integrations"), Some(Screen::Integrations));
        assert_eq!(screen("voice-key"), Some(Screen::Voice));
        assert_eq!(screen("live-voice"), Some(Screen::Voice));
        assert_eq!(screen("service-firmware"), Some(Screen::Service));
        assert_eq!(screen("live-service"), Some(Screen::Service));
        assert_eq!(screen("footprint-uninstall"), Some(Screen::Footprint));
        assert_eq!(screen("live-footprint"), Some(Screen::Footprint));
        assert_eq!(screen("local-api"), Some(Screen::LocalApi));
        assert_eq!(screen("live-local-api"), Some(Screen::LocalApi));
        assert_eq!(screen("menubar"), Some(Screen::MenuPreview));
        assert_eq!(screen("tokens"), Some(Screen::Kit));
        // The console's scenes are gone.
        for gone in ["deck", "confirm", "images", "usage-fable", "welcome-1"] {
            assert_eq!(screen(gone), None, "{gone}");
        }
    }

    #[test]
    fn a_walk_is_the_boards_synthetic_or_a_test_service_live_and_never_the_installed_one() {
        assert_eq!(Walk::of(true, false), Ok(Walk::Boards));
        assert_eq!(
            Walk::of(true, true),
            Ok(Walk::Boards),
            "synthetic dials nothing, whatever else is set"
        );
        assert_eq!(Walk::of(false, true), Ok(Walk::Live));
        let why = Walk::of(false, false).unwrap_err();
        assert!(why.contains("never"), "{why}");
        for (scene, name) in SCENES {
            assert_ne!(
                Walk::Boards.draws(scene),
                Walk::Live.draws(scene),
                "{name} is one walk's"
            );
        }
    }

    #[test]
    fn a_walk_takes_every_scene_it_draws_or_the_ones_named_and_says_why_it_left_one_out() {
        let (boards, refused) = Walk::Boards.scenes(None);
        assert_eq!(boards.len(), 33);
        assert!(refused.is_empty());
        assert!(boards.iter().all(|(scene, _)| scene.fixture().is_some()));
        let (live, refused) = Walk::Live.scenes(None);
        assert!(refused.is_empty());
        assert_eq!(
            names(&live),
            [
                "live-deck",
                "live-faces",
                "live-integrations",
                "live-voice",
                "live-service",
                "live-footprint",
                "live-local-api",
            ]
        );
        // Named: in the walk's own order, and what it cannot take is said.
        let only: Vec<String> = ["voice", "live-deck", "main", "nope"]
            .into_iter()
            .map(String::from)
            .collect();
        let (named, refused) = Walk::Boards.scenes(Some(&only));
        assert_eq!(names(&named), ["main", "voice"]);
        assert_eq!(refused.len(), 2);
        assert!(refused[0].starts_with("live-deck: ") && refused[0].contains("DECKD_DATA_DIR"));
        assert!(refused[1].starts_with("nope: "));
        let (named, refused) = Walk::Live.scenes(Some(&only));
        assert_eq!(names(&named), ["live-deck"]);
        assert_eq!(refused.len(), 3);
        assert!(refused[0].starts_with("voice: ") && refused[0].contains("SYNTHETIC"));
        // The variable's own spelling.
        assert_eq!(
            only_named(Some(" main, voice ,,")),
            Some(vec!["main".to_string(), "voice".to_string()])
        );
        assert_eq!(only_named(Some(" , ")), None);
        assert_eq!(only_named(None), None);
    }

    #[test]
    fn the_size_and_the_language_are_read_as_the_variables_spell_them() {
        assert_eq!(size_named("1040x780"), Some(BOARD_SIZE));
        assert_eq!(size_named(" 880 x 560 "), Some((880., 560.)));
        for bad in ["1040", "x780", "1040x", "0x780", "-1x2", "wide x tall"] {
            assert_eq!(size_named(bad), None, "{bad}");
        }
        assert_eq!(language_named("zh-Hans"), Some(Language::SimplifiedChinese));
        assert_eq!(language_named(" ja"), Some(Language::Japanese));
        assert_eq!(language_named("en"), Some(Language::English));
        assert_eq!(language_named("fr"), None);
    }

    #[test]
    fn the_keys_are_read_as_steps_of_keystrokes_in_gpuis_spelling() {
        let steps = |value| keys_named(Some(value));
        assert_eq!(
            steps("tab; down  down ;; enter"),
            vec![vec!["tab"], vec!["down", "down"], vec!["enter"]]
        );
        assert_eq!(
            steps("alt-right;cmd-2"),
            vec![vec!["alt-right"], vec!["cmd-2"]]
        );
        assert!(steps(" ; ").is_empty());
        assert!(keys_named(None).is_empty());
        // Every keystroke the checks type is one gpui reads.
        let typed = "tab shift-tab down up left right home end enter space alt-left alt-right";
        for key in typed.split(' ').chain(["cmd-1", "cmd-5", "escape"]) {
            assert!(gpui::Keystroke::parse(key).is_ok(), "{key}");
        }
    }

    #[test]
    fn the_preflight_tells_a_drawn_window_from_one_that_was_not() {
        let image = |bytes: &[u8]| Ok(bytes.to_vec());
        assert_eq!(preflight_verdict(&[image(b"main"), image(b"tokens")]), 0);
        assert_eq!(
            preflight_verdict(&[image(b"blank"), image(b"blank")]),
            PREFLIGHT_SAME
        );
        assert_eq!(
            preflight_verdict(&[image(b"main"), Err("the capture came back empty".into())]),
            PREFLIGHT_FAILED
        );
        assert_eq!(preflight_verdict(&[image(b"main")]), PREFLIGHT_FAILED);
        assert_eq!(PREFLIGHT_SAME, 3);
        assert_ne!(PREFLIGHT_FAILED, PREFLIGHT_SAME);
    }

    #[test]
    fn the_board_mirror_is_the_main_boards_deck_and_never_a_session() {
        let board = board_snapshot();
        let device = &board.device;
        assert_eq!(
            (device.link, device.session),
            (Link::Connected, SessionState::Live)
        );
        assert_eq!(device.transport, Some(Transport::Bluetooth));
        assert_eq!((device.battery, device.charging), (Some(85), Some(false)));
        assert_eq!(
            present::fw_text(device.fw.as_deref().unwrap(), device.board.as_deref()),
            "0.13.0 \u{b7} Rev B"
        );
        // The Agents face up on the strip of seven, Agents and Settings at
        // the ends, every face by its template's own id.
        let strip: Vec<(u8, &str)> = device.faces.iter().map(|f| (f.f, f.n.as_str())).collect();
        assert_eq!(
            strip,
            [
                (1, "Agents"),
                (3, "Macropad"),
                (4, "Usage"),
                (7, "System"),
                (8, "Timer"),
                (6, "Arcade"),
                (5, "Settings"),
            ]
        );
        assert_eq!(device.face, Some(1));
        assert_eq!(board.faces, device.faces);
        // Every reconnect is in the hour before the boards' minute.
        assert_eq!(device.drops.len(), BOARD_DROPS as usize);
        assert_eq!(drops_last_hour(device, board_now()), BOARD_DROPS);
        assert!(device.drops.windows(2).all(|pair| pair[0].at < pair[1].at));
        assert!(device.last.as_ref().unwrap().at > device.drops.last().unwrap().at);
        // The Deck board's settings, each one the deck takes.
        let settings = board.settings;
        assert_eq!(
            (
                settings.speaker,
                settings.mic,
                settings.announce,
                settings.chime,
                settings.autojump,
                settings.brightness,
                settings.idlehome,
                settings.textsize,
                settings.language,
            ),
            (6, 7, 1, 1, 1, 4, 120, 1, 0)
        );
        for (key, value) in settings.iter() {
            assert!(key.accepts(value), "{key:?} = {value}");
        }
        assert_eq!(device.settings, settings);
        // The Macropad's eight keys; the screenshot key is touch only.
        let keys = &board.macros[&BOARD_MACROPAD].keys;
        let labels: Vec<&str> = keys.iter().map(|key| key.label.as_str()).collect();
        assert_eq!(
            labels,
            ["build", "test", "deploy", "git push", "screenshot", "lock", "mute", "term"]
        );
        assert_eq!(keys[4].binding, None);
        assert_eq!(keys[4].keys.as_deref(), Some("shift+cmd+4"));
        assert!(board.agents.is_empty(), "this window never shows a session");
        assert_eq!(board.voice.permissions.bluetooth, Permission::Granted);
        assert_eq!(board.voice.permissions.speech, Permission::NotDetermined);
    }

    #[test]
    fn the_boards_keys_and_strips_are_ones_the_service_takes_as_they_are() {
        let started = std::time::Instant::now();
        let mut store = Store::new(StoreConfig::new(started, board_now(), 1));
        let mut send = |action: UiAction| {
            let (method, params) = action.request();
            let request = serde_json::from_value::<Request>(
                serde_json::json!({"method": method, "params": params}),
            )
            .unwrap_or_else(|e| panic!("{method}: {e}"));
            let (result, _, _) = store.call(Principal::Uid(501), SessionId(1), request, started);
            result.unwrap_or_else(|e| panic!("{method}: {e}"));
        };
        send(UiAction::DeviceMacroSet {
            f: BOARD_MACROPAD,
            layout: "app".into(),
            src: None,
            keys: board_keys(),
        });
        for scene in [Scene::Main, Scene::FacesFull] {
            send(UiAction::DeviceFacesSet {
                list: scene.fixture().unwrap().mirror.faces,
            });
        }
        assert_eq!(store.snapshot().macros, board_snapshot().macros);
    }

    #[test]
    fn the_offline_mirror_remembers_the_deck_as_it_was_at_09_41() {
        crate::i18n::install();
        let offline = offline_snapshot();
        let device = &offline.device;
        assert_eq!(
            (device.link, device.session),
            (Link::Offline, SessionState::Absent)
        );
        assert_eq!(
            (device.fw.as_deref(), device.face, device.transport, device.battery),
            (None, None, None, None)
        );
        let last = device.last.as_ref().unwrap();
        assert_eq!(
            present::day_and_time(last.at, board_now(), Language::English),
            "today, 09:41"
        );
        assert_eq!(
            present::fw_text(last.fw.as_deref().unwrap(), last.board.as_deref()),
            "0.13.0 \u{b7} Rev B"
        );
        // What the service keeps for the deck stays with it.
        let board = board_snapshot();
        assert_eq!(
            (&offline.faces, &offline.macros, offline.settings),
            (&board.faces, &board.macros, board.settings)
        );
    }

    #[test]
    fn each_staged_scene_is_drawn_over_its_fixture() {
        let fixture = |scene: Scene| scene.fixture().expect("a staged scene");
        for (scene, name) in SCENES {
            assert_eq!(scene.fixture().is_none(), scene.live(), "{name}");
        }
        // The linked boards share one mirror, and a white deck.
        for scene in [
            Scene::Main,
            Scene::FacesKeys,
            Scene::FacesUsage,
            Scene::Integrations,
            Scene::Voice,
            Scene::Service,
            Scene::Footprint,
            Scene::MenuBar,
            Scene::Tokens,
            Scene::LinkDetails,
            Scene::FirstVoice,
        ] {
            assert_eq!(
                fixture(scene),
                Fixture {
                    mirror: board_snapshot(),
                    connected: true,
                    case: Case::White
                },
                "{scene:?}"
            );
        }
        assert_eq!(fixture(Scene::NotLinked).mirror, offline_snapshot());
        assert!(fixture(Scene::NotLinked).connected);
        let down = fixture(Scene::ServiceDown);
        assert_eq!((down.mirror, down.connected), (offline_snapshot(), false));
        // Nothing installed: no service has ever answered.
        for scene in [Scene::NotInstalled, Scene::Connect, Scene::Found] {
            let nothing = fixture(scene);
            assert_eq!(
                (nothing.mirror, nothing.connected, nothing.case),
                (Snapshot::default(), false, Case::White)
            );
        }
        // DeckQuiet: an ordinary day, and a black deck.
        let quiet = fixture(Scene::DeckQuiet);
        assert!(quiet.mirror.device.drops.is_empty());
        assert_eq!(quiet.case, Case::Black);
        // DeckZh speaks Simplified Chinese.
        let zh = fixture(Scene::DeckZh).mirror;
        assert_eq!(
            crate::shell::spoken(None, zh.settings.language),
            Language::SimplifiedChinese
        );
        // By cable, the charge and the face unsaid.
        let unknown = fixture(Scene::DeckUnknown).mirror.device;
        assert_eq!(unknown.transport, Some(Transport::Usb));
        assert_eq!((unknown.battery, unknown.charging, unknown.face), (None, None, None));
        // The strip full: eight faces, Omarchy second.
        let full = fixture(Scene::FacesFull).mirror;
        assert_eq!(full.faces.len(), 8);
        assert_eq!(full.faces[1].t, Template::Omarchy);
        assert_eq!(full.device.faces, full.faces);
    }

    #[test]
    fn the_menu_bar_board_is_what_the_board_mirror_says() {
        let none = Extras::default();
        let board = board_snapshot();
        let list = attention(&board, true, board_now(), &none);
        assert_eq!(list, vec![Attention::LinkDrops { count: 41 }]);
        let absent = deck_gui_core::NoService::NotRunning;
        let menu = MenuModel::of(&board, true, absent, Some(false), &list, Language::English);
        assert_eq!(menu.icon, IconState::Care);
        assert_eq!(menu.status, "Linked over Bluetooth \u{b7} 85%");
        assert!(menu.link_problem && menu.announce);
        // DeckQuiet: nothing to look at.
        let quiet = Scene::DeckQuiet.fixture().unwrap().mirror;
        assert!(attention(&quiet, true, board_now(), &none).is_empty());
        // NotLinked: the deck is away.
        let offline = offline_snapshot();
        let list = attention(&offline, true, board_now(), &none);
        let menu = MenuModel::of(&offline, true, absent, None, &list, Language::English);
        assert_eq!((menu.icon, menu.status.as_str()), (IconState::NotLinked, "Not linked"));
    }
}
