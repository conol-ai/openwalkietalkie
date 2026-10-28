//! The tray's **aggregate**, its menu ids and what each item does (§8.2's
//! tray row) — the whole of the tray that can be tested without a menu bar.
//!
//! The status item itself is `openwalkietalkie-app`'s (`tray-icon` is a macOS
//! binding and this crate has no platform code at all), but nothing about
//! *what it says* lives there. The dot, the tooltip, the two toggles' checked
//! state and the action every click turns into are computed here, from the
//! same mirror the window draws, so the tray and the window can never
//! disagree about how many questions are waiting.
//!
//! One rule shapes the rest: **the tray is a view of the mirror, not a
//! second state.** [`Summary::of`] is a pure function of the snapshot, and a
//! click is a [`TrayCommand`] the window turns into a [`UiAction`] — the
//! same path a button on a screen takes. A tray that flipped its own
//! checkmark on click would be showing the operator a setting the daemon had
//! refused.
use crate::{ConfigValue, UiAction};
use deck_model::{AgentState, SetKey, Snapshot, Tone, DISCONNECTED};

/// The menu, in the order §8.2 lists it. The ids are the strings `muda`
/// hands back on a click, so they are the wire between the menu and
/// [`TrayCommand::parse`].
pub const OPEN: &str = "tray-open";
pub const MUTE: &str = "tray-mute";
pub const PAUSE_HOOKS: &str = "tray-pause-hooks";
pub const UPDATES: &str = "tray-updates";
pub const QUIT: &str = "tray-quit";
pub const STOP_SERVICE: &str = "tray-stop-service";

/// What a click on the tray asks for.
///
/// Four of the six are ordinary calls and two are not: [`TrayCommand::Open`]
/// is a window action and [`TrayCommand::Quit`] ends **this app** and
/// nothing else — the daemon is a launchd agent and keeps running, which is
/// why [`QUIT_TOOLTIP`] exists and why the two are separate items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayCommand {
    Open,
    /// `device.set{k:"announce"}` — the deck's spoken replies, on or off.
    Mute,
    /// `config.set{hooks.enabled}`. A **hot** key (§3.7), so this is a real
    /// pause and not a restart: the running daemon picks the new value up
    /// through the same notify + `ArcSwap` reload a hand-edited
    /// `config.toml` goes through.
    PauseHooks,
    CheckUpdates,
    /// Quit the GUI. The service keeps running.
    Quit,
    /// `service.stop` — the service, and with it the deck.
    StopService,
}

impl TrayCommand {
    pub const ALL: [TrayCommand; 6] = [
        TrayCommand::Open,
        TrayCommand::Mute,
        TrayCommand::PauseHooks,
        TrayCommand::CheckUpdates,
        TrayCommand::Quit,
        TrayCommand::StopService,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            TrayCommand::Open => OPEN,
            TrayCommand::Mute => MUTE,
            TrayCommand::PauseHooks => PAUSE_HOOKS,
            TrayCommand::CheckUpdates => UPDATES,
            TrayCommand::Quit => QUIT,
            TrayCommand::StopService => STOP_SERVICE,
        }
    }

    /// The id a click carried, as a command. `None` for an id this build
    /// does not know, which is a menu item it did not build.
    pub fn parse(id: &str) -> Option<TrayCommand> {
        TrayCommand::ALL.into_iter().find(|c| c.id() == id)
    }

    pub const fn label(self) -> &'static str {
        match self {
            TrayCommand::Open => "Open OpenWalkieTalkie",
            TrayCommand::Mute => "Mute announcements",
            TrayCommand::PauseHooks => "Pause hooks",
            TrayCommand::CheckUpdates => "Check for Updates\u{2026}",
            TrayCommand::Quit => "Quit OpenWalkieTalkie",
            TrayCommand::StopService => "Stop service",
        }
    }

    /// Whether the item is drawn with a tick rather than as a plain row.
    /// Both toggles show what the **daemon** last reported, never what was
    /// last clicked.
    pub const fn is_toggle(self) -> bool {
        matches!(self, TrayCommand::Mute | TrayCommand::PauseHooks)
    }
}

/// §8.2: "Quit (GUI only — tooltip says the service keeps running)". The
/// deck stays on the desk answering cards after this menu item; saying so
/// where the item is is the difference between quitting a window and
/// stopping the thing that answers.
pub const QUIT_TOOLTIP: &str =
    "Quits this window only. The service keeps running and the deck keeps working \u{2014} use \
     Stop service to stop that too.";

/// What the tray shows, as a pure function of the mirror.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// §8.2's aggregate dot: red question, blue working, green ready, grey
    /// none, **amber** whenever the service is degraded.
    pub dot: Tone,
    /// The tooltip's first line.
    pub headline: String,
    /// The second, when there is something to add.
    pub detail: Option<String>,
    /// `announce` is 0: the deck speaks nothing. The Mute item's tick.
    pub muted: bool,
    /// `[hooks] enabled` as `config.get` last answered. `None` until it has
    /// been read once, and the item is drawn unticked and disabled rather
    /// than claiming hooks are on.
    pub hooks_enabled: Option<bool>,
    /// `service.degraded`'s reason, verbatim.
    pub degraded: Option<String>,
    /// Who holds the port while the reason is `hooks_hijack_risk` — the
    /// squatter §4.1's one-time notification has to name.
    pub squatter: Option<String>,
}

impl Summary {
    /// The tray as this snapshot leaves it.
    ///
    /// `connected` is the socket and not the mirror: a window whose daemon
    /// has gone still holds the last roster (greyed, §8.2), and a tray that
    /// went on showing a green dot for it would be reporting a service that
    /// is not there. So a dropped socket is grey, whatever the last roster
    /// said.
    pub fn of(snapshot: &Snapshot, connected: bool, hooks_enabled: Option<bool>) -> Summary {
        let degraded = snapshot.service.degraded.clone();
        let squatter = snapshot
            .service
            .listener
            .as_ref()
            .and_then(|l| l.command.clone());
        let muted = snapshot.settings.announce == 0;
        if !connected {
            return Summary {
                dot: Tone::Gray,
                headline: "Service not running".into(),
                detail: Some("The deck is not being served. Open to start it.".into()),
                muted,
                hooks_enabled,
                // A degraded reason from before the socket dropped is about
                // a daemon that is no longer answering; the dot above
                // already says the worse thing.
                degraded: None,
                squatter: None,
            };
        }
        let (questions, working, ready) = counts(snapshot);
        // Amber first: a degraded service is the one thing the operator
        // cannot see from the roster, and §4.1's hijack case is sending
        // prompts and the `hooks` token to a stranger while it is true.
        let dot = match () {
            () if degraded.is_some() => Tone::Amber,
            () if questions > 0 => Tone::Red,
            () if working > 0 => Tone::Blue,
            () if ready > 0 => Tone::Green,
            () => Tone::Gray,
        };
        Summary {
            dot,
            headline: headline(questions, working, ready),
            detail: degraded.as_deref().map(|reason| match (reason, &squatter) {
                ("hooks_hijack_risk", Some(who)) => format!(
                    "Port {} is held by {who}; Claude hooks may stall",
                    snapshot.service.port
                ),
                ("hooks_hijack_risk", None) => format!(
                    "Port {} is held by another process; Claude hooks may stall",
                    snapshot.service.port
                ),
                (other, _) => format!("Service degraded: {other}"),
            }),
            muted,
            hooks_enabled,
            degraded,
            squatter,
        }
    }

    /// The whole tooltip, headline and detail.
    pub fn tooltip(&self) -> String {
        match &self.detail {
            Some(detail) => format!("OpenWalkieTalkie \u{2014} {}\n{detail}", self.headline),
            None => format!("OpenWalkieTalkie \u{2014} {}", self.headline),
        }
    }

    /// §8.2's amber: a degraded service, of which `hooks_hijack_risk` is the
    /// one that earns a notification of its own.
    pub fn wants_attention(&self) -> bool {
        self.degraded.is_some()
    }

    /// The action the **Mute** item is asking for: the opposite of what the
    /// mirror says, because a toggle toggles what is, not what was clicked
    /// last.
    pub fn mute_action(&self) -> UiAction {
        UiAction::DeviceSet {
            key: SetKey::Announce,
            value: u16::from(self.muted),
        }
    }

    /// The action **Pause hooks** is asking for, or `None` while
    /// `config.get` has not answered: a tray that guessed `false` here would
    /// re-enable hooks the operator had paused in `config.toml` by hand.
    pub fn pause_hooks_action(&self) -> Option<UiAction> {
        Some(UiAction::ConfigSet {
            key: "hooks.enabled".into(),
            value: ConfigValue::Bool(!self.hooks_enabled?),
        })
    }
}

/// Questions, working and ready, counting only agents whose session is still
/// there. A stale agent is grey on the deck and in the window
/// (`present::state_tone`), and it must not colour the tray either: a red dot
/// for a question nobody is waiting on is the one thing worse than no dot.
fn counts(snapshot: &Snapshot) -> (usize, usize, usize) {
    let live = snapshot
        .agents
        .values()
        .filter(|a| a.title.as_deref() != Some(DISCONNECTED));
    let mut counts = (0, 0, 0);
    for agent in live {
        match agent.state {
            AgentState::Question => counts.0 += 1,
            AgentState::Working => counts.1 += 1,
            AgentState::Ready => counts.2 += 1,
            AgentState::Error | AgentState::Idle => {}
        }
    }
    counts
}

fn headline(questions: usize, working: usize, ready: usize) -> String {
    let mut parts = Vec::new();
    for (n, one, many) in [
        (questions, "1 question", "questions"),
        (working, "1 working", "working"),
        (ready, "1 ready", "ready"),
    ] {
        match n {
            0 => {}
            1 => parts.push(one.to_string()),
            n => parts.push(format!("{n} {many}")),
        }
    }
    if parts.is_empty() {
        return "Nothing waiting".to_string();
    }
    parts.join(" \u{b7} ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use deck_model::{
        AdapterKind, Agent, AgentId, AnswerCap, Caps, Kind, Listener, OrderCap, Owner, Principal,
        SessionId, SessionInfo, Source, TitleSource, Ts,
    };
    use serde_json::Value;

    fn agent(id: &str, state: AgentState) -> Agent {
        Agent {
            id: AgentId(id.into()),
            slot: None,
            kind: Kind::Claude,
            name: id.into(),
            state,
            title: Some("Compiling openmicro".into()),
            title_source: TitleSource::Prompt,
            since: Ts::from_millis(0),
            source: Source::Hook,
            owner: Owner {
                principal: Principal::Adapter(AdapterKind::Claude),
                session: SessionId(7),
                lease_expires: None,
            },
            session: SessionInfo::default(),
            caps: Caps {
                answer: AnswerCap::Hold,
                order: OrderCap::Inject,
                focus: true,
            },
            companion: None,
            card: None,
            reply: None,
            messages: Vec::new(),
            workflow_runs: Vec::new(), workflow: None,
            chat_rev: 0,
            reply_rev: 0,
            voice: None,
            pinned: false,
            meta: Value::Null,
            redacted: false,
        }
    }

    fn snapshot(states: &[(&str, AgentState)]) -> Snapshot {
        let mut snapshot = Snapshot {
            rev: 4,
            ..Snapshot::default()
        };
        snapshot.service.port = 47_201;
        for (id, state) in states {
            snapshot
                .agents
                .insert(AgentId((*id).into()), agent(id, *state));
        }
        snapshot
    }

    #[test]
    fn the_aggregate_dot_is_the_worst_thing_on_the_roster_and_amber_beats_all_of_it() {
        let ready = snapshot(&[("a", AgentState::Ready)]);
        assert_eq!(Summary::of(&ready, true, None).dot, Tone::Green);
        let working = snapshot(&[("a", AgentState::Ready), ("b", AgentState::Working)]);
        assert_eq!(Summary::of(&working, true, None).dot, Tone::Blue);
        let question = snapshot(&[
            ("a", AgentState::Ready),
            ("b", AgentState::Working),
            ("c", AgentState::Question),
        ]);
        assert_eq!(Summary::of(&question, true, None).dot, Tone::Red);
        assert_eq!(Summary::of(&snapshot(&[]), true, None).dot, Tone::Gray);
        // §4.1: a degraded service is what the roster cannot show, so it
        // takes the dot off whatever else is true.
        let mut degraded = question.clone();
        degraded.service.degraded = Some("hooks_hijack_risk".into());
        assert_eq!(Summary::of(&degraded, true, None).dot, Tone::Amber);
    }

    #[test]
    fn a_service_that_is_not_answering_is_grey_whatever_the_last_roster_said() {
        let busy = snapshot(&[("a", AgentState::Question)]);
        let summary = Summary::of(&busy, false, Some(true));
        assert_eq!(summary.dot, Tone::Gray);
        assert_eq!(summary.headline, "Service not running");
        assert!(summary.tooltip().contains("Service not running"));
        // A reason from before the drop is about a daemon that has gone.
        let mut degraded = busy.clone();
        degraded.service.degraded = Some("port_in_use".into());
        assert_eq!(Summary::of(&degraded, false, None).dot, Tone::Gray);
        assert!(!Summary::of(&degraded, false, None).wants_attention());
    }

    #[test]
    fn a_stale_agent_colours_nothing_because_nobody_is_waiting_on_it() {
        let mut snapshot = snapshot(&[("a", AgentState::Question)]);
        snapshot
            .agents
            .get_mut(&AgentId("a".into()))
            .expect("the agent")
            .title = Some(DISCONNECTED.to_string());
        let summary = Summary::of(&snapshot, true, None);
        assert_eq!(summary.dot, Tone::Gray);
        assert_eq!(summary.headline, "Nothing waiting");
    }

    #[test]
    fn the_hijack_tooltip_names_the_squatter_and_the_port_it_holds() {
        let mut snapshot = snapshot(&[("a", AgentState::Ready)]);
        snapshot.service.degraded = Some("hooks_hijack_risk".into());
        snapshot.service.listener = Some(Listener {
            pid: 812,
            command: Some("node /usr/local/bin/other".into()),
        });
        let summary = Summary::of(&snapshot, true, Some(true));
        assert_eq!(summary.dot, Tone::Amber);
        assert!(summary.wants_attention());
        let detail = summary.detail.clone().expect("a degraded detail");
        assert!(detail.contains("47201"), "{detail}");
        assert!(detail.contains("node /usr/local/bin/other"), "{detail}");
        assert!(detail.contains("may stall"), "{detail}");
        assert_eq!(
            summary.squatter.as_deref(),
            Some("node /usr/local/bin/other")
        );
        // `lsof` told the daemon nothing: the port is still somebody's.
        snapshot.service.listener = None;
        let blind = Summary::of(&snapshot, true, None);
        assert!(blind
            .detail
            .expect("still degraded")
            .contains("another process"));
        // Any other reason is said in the daemon's own word.
        snapshot.service.degraded = Some("port_in_use".into());
        assert!(Summary::of(&snapshot, true, None)
            .detail
            .expect("a reason")
            .contains("port_in_use"));
    }

    #[test]
    fn both_toggles_ask_for_the_opposite_of_what_the_daemon_last_reported() {
        let mut snapshot = snapshot(&[("a", AgentState::Ready)]);
        // `announce` is 1 by default: the deck speaks, so Mute is unticked
        // and asks for 0.
        let speaking = Summary::of(&snapshot, true, Some(true));
        assert!(!speaking.muted);
        assert_eq!(
            speaking.mute_action(),
            UiAction::DeviceSet {
                key: SetKey::Announce,
                value: 0
            }
        );
        snapshot.settings.announce = 0;
        let muted = Summary::of(&snapshot, true, Some(false));
        assert!(muted.muted);
        assert_eq!(
            muted.mute_action(),
            UiAction::DeviceSet {
                key: SetKey::Announce,
                value: 1
            }
        );
        // Pause hooks is `config.set{hooks.enabled}` and it, too, is the
        // opposite of what was read.
        assert_eq!(
            Summary::of(&snapshot, true, Some(true)).pause_hooks_action(),
            Some(UiAction::ConfigSet {
                key: "hooks.enabled".into(),
                value: ConfigValue::Bool(false)
            })
        );
        assert_eq!(
            Summary::of(&snapshot, true, Some(false)).pause_hooks_action(),
            Some(UiAction::ConfigSet {
                key: "hooks.enabled".into(),
                value: ConfigValue::Bool(true)
            })
        );
        // Unread: the item cannot say what it would do, so it asks nothing.
        assert_eq!(
            Summary::of(&snapshot, true, None).pause_hooks_action(),
            None
        );
    }

    #[test]
    fn every_menu_id_round_trips_and_quit_says_the_service_keeps_running() {
        for command in TrayCommand::ALL {
            assert_eq!(TrayCommand::parse(command.id()), Some(command));
            assert!(command.id().starts_with("tray-"));
            assert!(!command.label().is_empty());
        }
        assert_eq!(TrayCommand::ALL.len(), 6);
        assert_eq!(TrayCommand::parse("tray-nothing"), None);
        assert!(TrayCommand::Mute.is_toggle() && TrayCommand::PauseHooks.is_toggle());
        assert!(!TrayCommand::Quit.is_toggle());
        // §8.2's requirement in as many words.
        assert!(QUIT_TOOLTIP.contains("service keeps running"));
        assert!(QUIT_TOOLTIP.contains("Stop service"));
    }

    #[test]
    fn the_headline_counts_what_is_waiting_and_says_so_in_one_line() {
        assert_eq!(headline(0, 0, 0), "Nothing waiting");
        assert_eq!(headline(1, 0, 0), "1 question");
        assert_eq!(headline(2, 1, 0), "2 questions \u{b7} 1 working");
        assert_eq!(headline(0, 0, 3), "3 ready");
    }
}
