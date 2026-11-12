//! The companion's status item: its icon, its status line, and what each
//! click on its menu asks for — the whole of the menu that can be tested
//! without a menu bar.
//!
//! One rule shapes the rest: **the menu is a view of the mirror, not a
//! second state.** [`MenuModel::of`] is a pure function of the snapshot, the
//! service's answer about the pause, the [`crate::attention`] list and, with
//! no service answering, why not ([`NoService`], which the window reads from
//! macOS); a click is a [`MenuCommand`] the window turns into the
//! [`UiAction`] the model names. A tick that flipped itself on click would
//! be showing the owner a setting the service had not taken.
use crate::attention::Attention;
use crate::i18n::Language;
use crate::UiAction;
use deck_model::{Link, SetKey, Snapshot, Transport};

/// The menu's item ids, in the order the menu lists them. They are the
/// strings `muda` hands back on a click, so they are the wire between the
/// menu and [`MenuCommand::parse`].
pub const LINK_PROBLEM: &str = "menu-link-problem";
pub const ANNOUNCE: &str = "menu-announce";
pub const PAUSE: &str = "menu-pause";
pub const OPEN: &str = "menu-open";
pub const UPDATES: &str = "menu-updates";
pub const QUIT: &str = "menu-quit";

/// What a click on the menu asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuCommand {
    /// Open the window on the Deck page with the link's details. Present
    /// only while the link has a problem ([`MenuModel::link_problem`]).
    ShowLinkProblem,
    /// `device.set{k:"announce"}`: the deck speaks replies, or does not.
    AutoAnnounce,
    /// Every source paused, or resumed ([`crate::care::pause_action`]).
    PauseIntegrations,
    Open,
    CheckUpdates,
    /// Quit this app. The service keeps running and the deck keeps working.
    Quit,
}

impl MenuCommand {
    /// In menu order.
    pub const ALL: [MenuCommand; 6] = [
        MenuCommand::ShowLinkProblem,
        MenuCommand::AutoAnnounce,
        MenuCommand::PauseIntegrations,
        MenuCommand::Open,
        MenuCommand::CheckUpdates,
        MenuCommand::Quit,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            MenuCommand::ShowLinkProblem => LINK_PROBLEM,
            MenuCommand::AutoAnnounce => ANNOUNCE,
            MenuCommand::PauseIntegrations => PAUSE,
            MenuCommand::Open => OPEN,
            MenuCommand::CheckUpdates => UPDATES,
            MenuCommand::Quit => QUIT,
        }
    }

    /// The id a click carried, as a command. `None` for an id this build
    /// does not know, which is an item it did not build.
    pub fn parse(id: &str) -> Option<MenuCommand> {
        MenuCommand::ALL.into_iter().find(|c| c.id() == id)
    }

    /// The English label, which is also its key in the tables.
    pub const fn label(self) -> &'static str {
        match self {
            MenuCommand::ShowLinkProblem => "Show Link Problem\u{2026}",
            MenuCommand::AutoAnnounce => "Auto-Announce",
            MenuCommand::PauseIntegrations => "Pause Integrations",
            MenuCommand::Open => "Open OpenWalkieTalkie",
            MenuCommand::CheckUpdates => "Check for Updates\u{2026}",
            MenuCommand::Quit => "Quit OpenWalkieTalkie",
        }
    }

    pub fn label_localized(self, language: Language) -> &'static str {
        language.text(self.label())
    }

    /// Whether the item is drawn with a tick: both show what the service
    /// last reported, never what was last clicked.
    pub const fn is_toggle(self) -> bool {
        matches!(
            self,
            MenuCommand::AutoAnnounce | MenuCommand::PauseIntegrations
        )
    }
}

/// Why no service answers, as far as the window can tell: the app reads
/// macOS's own record of the service's launch agent and says which this is
/// (the menu's status line, the frame's strip, the Deck and Service pages).
/// A run that cannot tell — anything but the installed app — says it is not
/// running.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NoService {
    /// It is set up and does not run: **Start**, **Open Log**.
    #[default]
    NotRunning,
    /// It was never set up on this Mac: **Set Up…** is first run's.
    NotInstalled,
    /// It is set up, and macOS holds it until the owner allows it under
    /// Login Items in System Settings: **Open Settings** goes there.
    AwaitingApproval,
}

/// Which of the three status-item icons to draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconState {
    /// The deck is linked and nothing needs attention.
    Linked,
    /// No deck, or no service to link one.
    NotLinked,
    /// Linked, and something on the attention list.
    Care,
}

/// What the status item shows, as a pure function of the mirror.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuModel {
    pub icon: IconState,
    /// The menu's first line, disabled: how the deck is linked and its
    /// charge, or why it is not.
    pub status: String,
    /// The link has a problem worth its details: **Show Link Problem…** is
    /// in the menu.
    pub link_problem: bool,
    /// The deck speaks replies (`announce` is on): **Auto-Announce**'s tick.
    pub announce: bool,
    /// **Pause Integrations**' tick as the service last answered. `None`
    /// until it has, and the item is disabled rather than guessing.
    pub paused: Option<bool>,
}

impl MenuModel {
    /// The status item as this snapshot leaves it.
    ///
    /// `connected` is the socket and not the mirror: a window whose service
    /// has gone still holds its last snapshot, and a menu that went on
    /// reading it would report a link nothing is serving. `absent` says why
    /// no service answers, and is read only while none does. `paused` is
    /// what the Integrations page read from `config.get`
    /// ([`crate::care::SourcesConfig::effective_pause`]), and `attention`
    /// is [`crate::attention::attention`]'s list for the same snapshot.
    pub fn of(
        snapshot: &Snapshot,
        connected: bool,
        absent: NoService,
        paused: Option<bool>,
        attention: &[Attention],
        language: Language,
    ) -> MenuModel {
        let linked = connected && snapshot.device.link != Link::Offline;
        let icon = if !linked {
            IconState::NotLinked
        } else if !attention.is_empty() {
            IconState::Care
        } else {
            IconState::Linked
        };
        MenuModel {
            icon,
            status: status(snapshot, connected, absent, language),
            link_problem: connected && attention.iter().any(Attention::is_link_problem),
            announce: snapshot.settings.announce != 0,
            paused,
        }
    }

    /// What **Auto-Announce** asks for: the opposite of what the mirror
    /// says, because a toggle toggles what is, not what was clicked last.
    pub fn announce_action(&self) -> UiAction {
        UiAction::DeviceSet {
            key: SetKey::Announce,
            value: u16::from(!self.announce),
        }
    }

    /// What **Pause Integrations** asks for, or `None` while the pause is
    /// unread: a menu that guessed would resume what the owner had paused.
    pub fn pause_action(&self) -> Option<UiAction> {
        Some(crate::care::pause_action(!self.paused?))
    }
}

/// The status line: how the deck is linked and its charge, as much of it
/// as the deck has said; or, with no service answering, why not.
fn status(snapshot: &Snapshot, connected: bool, absent: NoService, language: Language) -> String {
    if !connected {
        return match absent {
            NoService::NotRunning => language.text("Service not running"),
            NoService::NotInstalled => language.text("Service not installed"),
            NoService::AwaitingApproval => language.text("Service waiting for approval"),
        }
        .into();
    }
    let device = &snapshot.device;
    if device.link == Link::Offline {
        return language.text("Not linked").into();
    }
    let percent = device.battery.map(|battery| battery.to_string());
    match (device.transport, percent) {
        (Some(Transport::Bluetooth), Some(percent)) => {
            language.format("Linked over Bluetooth \u{b7} {0}%", &[&percent])
        }
        (Some(Transport::Bluetooth), None) => language.text("Linked over Bluetooth").into(),
        (Some(Transport::Usb), Some(percent)) => {
            language.format("Linked by cable \u{b7} {0}%", &[&percent])
        }
        (Some(Transport::Usb), None) => language.text("Linked by cable").into(),
        (None, Some(percent)) => language.format("Linked \u{b7} {0}%", &[&percent]),
        (None, None) => language.text("Linked").into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ConfigValue;

    fn linked(transport: Option<Transport>, battery: Option<u8>) -> Snapshot {
        let mut snapshot = Snapshot::default();
        snapshot.device.link = Link::Connected;
        snapshot.device.transport = transport;
        snapshot.device.battery = battery;
        snapshot
    }

    fn model(snapshot: &Snapshot, connected: bool, attention: &[Attention]) -> MenuModel {
        MenuModel::of(
            snapshot,
            connected,
            NoService::NotRunning,
            None,
            attention,
            Language::English,
        )
    }

    #[test]
    fn the_status_line_says_how_the_deck_is_linked_and_what_it_has_said_of_its_charge() {
        for (transport, battery, line) in [
            (Some(Transport::Bluetooth), Some(85), "Linked over Bluetooth \u{b7} 85%"),
            (Some(Transport::Bluetooth), None, "Linked over Bluetooth"),
            (Some(Transport::Usb), Some(100), "Linked by cable \u{b7} 100%"),
            (Some(Transport::Usb), None, "Linked by cable"),
            (None, Some(7), "Linked \u{b7} 7%"),
            (None, None, "Linked"),
        ] {
            let menu = model(&linked(transport, battery), true, &[]);
            assert_eq!(menu.status, line);
            assert_eq!(menu.icon, IconState::Linked, "{line}");
            assert!(!menu.link_problem);
        }
        // A deck that is linked and silent is still linked, and says so.
        let mut silent = linked(Some(Transport::Usb), None);
        silent.device.link = Link::Degraded;
        assert_eq!(model(&silent, true, &[]).status, "Linked by cable");
    }

    #[test]
    fn no_deck_and_no_service_each_say_so_and_draw_the_not_linked_icon() {
        let mut gone = linked(Some(Transport::Bluetooth), Some(85));
        gone.device.link = Link::Offline;
        let menu = model(&gone, true, &[Attention::FirmwareUpdate { version: "0.14.0".into() }]);
        assert_eq!(menu.status, "Not linked");
        assert_eq!(menu.icon, IconState::NotLinked, "no deck is never care");
        // The service has gone: whatever its last snapshot said.
        let stale = linked(Some(Transport::Bluetooth), Some(85));
        let menu = model(&stale, false, &[Attention::NotAnswering]);
        assert_eq!(menu.status, "Service not running");
        assert_eq!(menu.icon, IconState::NotLinked);
        assert!(!menu.link_problem, "no details from a service that is not there");
    }

    #[test]
    fn no_service_says_whether_it_is_not_running_not_installed_or_waiting_for_approval() {
        let stale = linked(Some(Transport::Bluetooth), Some(85));
        let line = |connected: bool, absent: NoService| {
            MenuModel::of(&stale, connected, absent, None, &[], Language::English).status
        };
        assert_eq!(line(false, NoService::NotRunning), "Service not running");
        assert_eq!(line(false, NoService::NotInstalled), "Service not installed");
        assert_eq!(
            line(false, NoService::AwaitingApproval),
            "Service waiting for approval"
        );
        assert_eq!(NoService::default(), NoService::NotRunning);
        // Why it is absent is read only while it is: a service that answers
        // says how the deck is linked, whatever the window last thought.
        for absent in [
            NoService::NotRunning,
            NoService::NotInstalled,
            NoService::AwaitingApproval,
        ] {
            assert_eq!(line(true, absent), "Linked over Bluetooth \u{b7} 85%");
            let menu = MenuModel::of(&stale, false, absent, Some(true), &[], Language::English);
            assert_eq!(menu.icon, IconState::NotLinked, "{absent:?}");
        }
    }

    #[test]
    fn another_program_on_the_local_apis_port_leaves_the_menu_linked_and_without_care() {
        for reason in [crate::admin::PORT_IN_USE, crate::admin::HIJACK_RISK] {
            let mut snapshot = linked(Some(Transport::Usb), Some(60));
            snapshot.service.degraded = Some(reason.into());
            let list = crate::attention::attention(
                &snapshot,
                true,
                deck_model::Ts(0),
                &crate::attention::Extras::default(),
            );
            let menu = model(&snapshot, true, &list);
            assert_eq!(menu.icon, IconState::Linked, "{reason}");
            assert!(!menu.link_problem, "{reason}");
            assert_eq!(menu.status, "Linked by cable \u{b7} 60%");
        }
    }

    #[test]
    fn anything_on_the_attention_list_is_care_and_only_the_links_own_problems_offer_details() {
        let snapshot = linked(Some(Transport::Bluetooth), Some(85));
        let firmware = model(
            &snapshot,
            true,
            &[Attention::FirmwareUpdate { version: "0.14.0".into() }],
        );
        assert_eq!(firmware.icon, IconState::Care);
        assert!(!firmware.link_problem);
        for problem in [Attention::LinkDrops { count: 41 }, Attention::NotAnswering] {
            let menu = model(&snapshot, true, std::slice::from_ref(&problem));
            assert_eq!(menu.icon, IconState::Care, "{problem:?}");
            assert!(menu.link_problem, "{problem:?}");
        }
        // A deck that dropped often and is gone now: the details still say
        // how, though the icon is the not-linked one.
        let mut gone = snapshot.clone();
        gone.device.link = Link::Offline;
        let menu = model(&gone, true, &[Attention::LinkDrops { count: 5 }]);
        assert_eq!((menu.icon, menu.link_problem), (IconState::NotLinked, true));
    }

    #[test]
    fn auto_announce_ticks_what_the_deck_does_and_asks_for_the_opposite() {
        let mut snapshot = linked(None, None);
        // `announce` is on by default: ticked, and a click turns it off.
        let speaking = model(&snapshot, true, &[]);
        assert!(speaking.announce);
        assert_eq!(
            speaking.announce_action(),
            UiAction::DeviceSet {
                key: SetKey::Announce,
                value: 0
            }
        );
        snapshot.settings.announce = 0;
        let quiet = model(&snapshot, true, &[]);
        assert!(!quiet.announce);
        assert_eq!(
            quiet.announce_action(),
            UiAction::DeviceSet {
                key: SetKey::Announce,
                value: 1
            }
        );
    }

    #[test]
    fn pause_asks_nothing_until_it_is_read_and_then_the_opposite_of_what_was_read() {
        let snapshot = linked(None, None);
        let of = |paused: Option<bool>| {
            MenuModel::of(
                &snapshot,
                true,
                NoService::NotRunning,
                paused,
                &[],
                Language::English,
            )
        };
        let unread = of(None);
        assert_eq!(unread.pause_action(), None);
        let running = of(Some(false));
        assert_eq!(
            running.pause_action(),
            Some(UiAction::ConfigPatch {
                entries: vec![("sources.paused".into(), ConfigValue::Bool(true))]
            })
        );
        let paused = of(Some(true));
        assert_eq!(paused.paused, Some(true));
        assert_eq!(
            paused.pause_action(),
            Some(UiAction::ConfigPatch {
                entries: vec![
                    ("sources.paused".into(), ConfigValue::Bool(false)),
                    ("hooks.enabled".into(), ConfigValue::Bool(true)),
                ]
            })
        );
    }

    #[test]
    fn every_menu_id_round_trips_and_every_label_is_translated() {
        assert_eq!(MenuCommand::ALL.len(), 6);
        for command in MenuCommand::ALL {
            assert_eq!(MenuCommand::parse(command.id()), Some(command));
            assert!(command.id().starts_with("menu-"));
            for language in Language::ALL {
                let label = command.label_localized(language);
                assert!(!label.is_empty());
                if matches!(command, MenuCommand::Open | MenuCommand::Quit) {
                    assert!(label.contains("OpenWalkieTalkie"), "{language:?}: {label}");
                }
                if language != Language::English {
                    assert_ne!(label, command.label(), "{language:?}: {command:?}");
                }
            }
        }
        assert_eq!(MenuCommand::parse("tray-open"), None);
        assert_eq!(MenuCommand::parse("menu-nothing"), None);
        let toggles: Vec<MenuCommand> = MenuCommand::ALL
            .into_iter()
            .filter(|c| c.is_toggle())
            .collect();
        assert_eq!(
            toggles,
            vec![MenuCommand::AutoAnnounce, MenuCommand::PauseIntegrations]
        );
    }

    #[test]
    fn every_status_line_is_translated_and_keeps_the_percent() {
        let mut gone = linked(None, None);
        gone.device.link = Link::Offline;
        let cases = [
            (linked(Some(Transport::Bluetooth), Some(85)), true, NoService::NotRunning),
            (linked(Some(Transport::Bluetooth), None), true, NoService::NotRunning),
            (linked(Some(Transport::Usb), Some(85)), true, NoService::NotRunning),
            (linked(Some(Transport::Usb), None), true, NoService::NotRunning),
            (linked(None, Some(85)), true, NoService::NotRunning),
            (linked(None, None), true, NoService::NotRunning),
            (gone, true, NoService::NotRunning),
            (linked(None, None), false, NoService::NotRunning),
            (linked(None, None), false, NoService::NotInstalled),
            (linked(None, None), false, NoService::AwaitingApproval),
        ];
        for (snapshot, connected, absent) in &cases {
            let english =
                MenuModel::of(snapshot, *connected, *absent, None, &[], Language::English).status;
            for language in [
                Language::SimplifiedChinese,
                Language::TraditionalChinese,
                Language::Japanese,
            ] {
                let line = MenuModel::of(snapshot, *connected, *absent, None, &[], language).status;
                assert_ne!(line, english, "{language:?}: {english} is not translated");
                assert_eq!(
                    line.contains("85%"),
                    english.contains("85%"),
                    "{language:?}: {line}"
                );
            }
        }
    }
}
