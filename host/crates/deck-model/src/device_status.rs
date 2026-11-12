//! The deck's status (`device.status`): the bus it is linked on, its charge,
//! its board, the deck as it was last linked, and the link's recent drops.
//!
//! The hid thread reports them as `Command::DeviceReport` beside its link
//! events. The six fields always travel together in one event, whichever of
//! them changed, so a reader replaces all six at once and never has to tell
//! "absent" from "unchanged".
use crate::{
    DeviceReport, Effect, EventKind, LastLinked, Link, LinkDrop, Principal, StatePatch, Store, Ts,
};
use std::time::Duration;

/// The most drops the status keeps.
pub const LINK_DROPS_KEPT: usize = 60;
/// How far back the drops reach: older ones go when another is added.
pub const LINK_DROP_WINDOW: Duration = Duration::from_secs(3600);

impl Store {
    pub(crate) fn on_report(&mut self, report: DeviceReport) {
        match report {
            DeviceReport::Dropped { why } => self.note_drop(why),
            // The other reports describe the linked deck. Once its link is
            // down the `Down` has cleared them, and a late one must not bring
            // a deck that is gone back.
            _ if self.snapshot.device.link == Link::Offline => {}
            report => {
                if !self.describe(report) {
                    return;
                }
                // The board is only asked for once the link is live, so the
                // remembered deck learns it here, keeping the moment it went
                // live: without this the not-linked page would name the
                // firmware and not the board.
                if self.live_link {
                    let at = self.snapshot.device.last.as_ref().map(|last| last.at);
                    self.remember_link(at.unwrap_or_else(|| self.ts_now()));
                }
                self.emit_status(Principal::Device);
            }
        }
    }

    /// Writes what `report` says about the linked deck; whether that changed
    /// anything.
    fn describe(&mut self, report: DeviceReport) -> bool {
        let dev = &mut self.snapshot.device;
        match report {
            DeviceReport::Transport(transport) => {
                std::mem::replace(&mut dev.transport, transport) != transport
            }
            DeviceReport::Power { battery, charging } => {
                // The deck sends 0 when it has no reading, and cannot tell
                // that from an empty cell, so 0 is unknown.
                let battery = battery.filter(|percent| *percent != 0).map(|p| p.min(100));
                let changed = (dev.battery, dev.charging) != (battery, charging);
                dev.battery = battery;
                dev.charging = charging;
                changed
            }
            DeviceReport::Board(board) => {
                let board = board
                    .map(|name| name.trim().to_string())
                    .filter(|name| !name.is_empty());
                let changed = dev.board != board;
                dev.board = board;
                changed
            }
            DeviceReport::Dropped { .. } => false,
        }
    }

    /// A link is about to be reported down. The drop waits for that `Down`,
    /// which records it in the one `device.status` that also clears the
    /// deck: one event for one drop, and nothing in the snapshot that no
    /// event has said.
    fn note_drop(&mut self, why: String) {
        // An install closes the link on purpose; nothing failed.
        if why == "firmware_update" {
            return;
        }
        let drop = LinkDrop {
            at: self.ts_now(),
            why,
        };
        if self.snapshot.device.link == Link::Offline {
            // No `Down` is coming to say this one.
            self.record_drop(drop);
            self.emit_status(Principal::Device);
        } else {
            self.pending_drop = Some(drop);
        }
    }

    /// Adds `drop` to the list, newest last, pruning what the window and the
    /// cap no longer hold.
    fn record_drop(&mut self, drop: LinkDrop) {
        let window = LINK_DROP_WINDOW.as_millis() as u64;
        let drops = &mut self.snapshot.device.drops;
        drops.retain(|old| drop.at.millis().saturating_sub(old.at.millis()) <= window);
        drops.push(drop);
        let excess = drops.len().saturating_sub(LINK_DROPS_KEPT);
        drops.drain(..excess);
    }

    /// The session went live (`Live`, or `Synced` after a barrier): the deck
    /// is remembered as it is now, stamped with the moment its link first
    /// went live. A link that recovers from misses or writes another barrier
    /// keeps that moment.
    pub(crate) fn status_link_live(&mut self) {
        let at = match &self.snapshot.device.last {
            Some(last) if self.live_link => last.at,
            _ => self.ts_now(),
        };
        self.live_link = true;
        if self.remember_link(at) {
            self.emit_status(Principal::Device);
        }
    }

    /// The link ended: a `Down`, or this service going away while it was
    /// up. The drop the hid thread reported just before is recorded; a deck
    /// that went live is remembered with the moment it was lost; what
    /// described the linked deck is cleared; one `device.status` says all of
    /// it. A link that never went live leaves the previous deck's record as
    /// it was.
    pub(crate) fn status_link_ended(&mut self, principal: Principal) {
        if let Some(drop) = self.pending_drop.take() {
            self.record_drop(drop);
        }
        if std::mem::take(&mut self.live_link) {
            let now = self.ts_now();
            self.remember_link(now);
        }
        let dev = &mut self.snapshot.device;
        dev.transport = None;
        dev.battery = None;
        dev.charging = None;
        dev.board = None;
        self.emit_status(principal);
    }

    /// `last` from the deck as it is now, stamped `at`, and persisted when
    /// that changed it. Whether it did.
    fn remember_link(&mut self, at: Ts) -> bool {
        let dev = &self.snapshot.device;
        let last = LastLinked {
            at,
            fw: dev.fw.clone(),
            board: dev.board.clone(),
            transport: dev.transport,
        };
        if dev.last.as_ref() == Some(&last) {
            return false;
        }
        self.snapshot.device.last = Some(last.clone());
        self.effects
            .push(Effect::Persist(StatePatch::LastLinked(last)));
        true
    }

    fn emit_status(&mut self, principal: Principal) {
        let dev = &self.snapshot.device;
        let data = serde_json::json!({
            "transport": dev.transport, "battery": dev.battery, "charging": dev.charging,
            "board": dev.board, "last": dev.last, "drops": dev.drops,
        });
        self.emit(EventKind::DeviceStatus, principal, data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Sim;
    use crate::{Command, DeviceLine, LinkEvent, Transport};
    use serde_json::json;

    const FW: &str = "0.13.0-openwalkietalkie";

    fn report(report: DeviceReport) -> Command {
        Command::DeviceReport(report)
    }

    fn link(event: LinkEvent) -> Command {
        Command::Link(event)
    }

    fn hello() -> Command {
        Command::Device(DeviceLine::Hello {
            v: 1,
            fw: FW.into(),
            sid: 314_159,
            boot: false,
            rx: crate::LINE_MAX_U16,
            agents: None,
            lang: None,
            lang_rev: None,
            faces: vec![],
        })
    }

    fn drop_(why: &str) -> Command {
        report(DeviceReport::Dropped { why: why.into() })
    }

    /// A deck opened on USB that answered its hello and went live now:
    /// the order the hid thread reports them in.
    fn live_on_usb(sim: &mut Sim) -> Ts {
        sim.apply(link(LinkEvent::Linked));
        sim.apply(report(DeviceReport::Transport(Some(Transport::Usb))));
        sim.apply(hello());
        sim.apply(link(LinkEvent::Live { sid: 314_159 }));
        sim.store.ts(sim.t)
    }

    fn persisted_last(sim: &Sim) -> Vec<LastLinked> {
        sim.effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Persist(StatePatch::LastLinked(last)) => Some(last.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn each_report_writes_its_field_once_and_an_unchanged_one_says_nothing() {
        let mut sim = Sim::new();
        sim.apply(link(LinkEvent::Linked));
        for each in [
            DeviceReport::Transport(Some(Transport::Bluetooth)),
            DeviceReport::Power {
                battery: Some(85),
                charging: Some(true),
            },
            DeviceReport::Board(Some("Rev B".into())),
        ] {
            sim.apply(report(each.clone()));
            assert_eq!(sim.kinds(), vec![EventKind::DeviceStatus], "{each:?}");
            sim.apply(report(each.clone()));
            assert!(sim.events.is_empty() && sim.effects.is_empty(), "{each:?} again");
        }
        let dev = &sim.store.snapshot().device;
        assert_eq!(dev.transport, Some(Transport::Bluetooth));
        assert_eq!((dev.battery, dev.charging), (Some(85), Some(true)));
        assert_eq!(dev.board.as_deref(), Some("Rev B"));
        // One field moved, and the event still carries all six.
        sim.apply(report(DeviceReport::Power {
            battery: Some(80),
            charging: Some(true),
        }));
        assert_eq!(sim.events.len(), 1);
        assert_eq!(sim.events[0].principal, Principal::Device);
        assert_eq!(sim.events[0].rev, sim.store.snapshot().rev);
        assert_eq!(
            sim.events[0].data,
            json!({"transport": "bluetooth", "battery": 80, "charging": true,
                   "board": "Rev B", "last": null, "drops": []})
        );
    }

    #[test]
    fn battery_zero_is_unknown_and_charging_is_what_the_deck_says() {
        let mut sim = Sim::new();
        sim.apply(link(LinkEvent::Linked));
        sim.apply(report(DeviceReport::Power {
            battery: Some(0),
            charging: Some(false),
        }));
        let dev = &sim.store.snapshot().device;
        assert_eq!((dev.battery, dev.charging), (None, Some(false)));
        assert_eq!(sim.events[0].data["battery"], json!(null));
        // No reading and a reading of 0 are the same unknown: nothing moved.
        sim.apply(report(DeviceReport::Power {
            battery: None,
            charging: Some(false),
        }));
        assert!(sim.events.is_empty());
        // A percent is never more than a hundred, and a deck that stops
        // saying whether it charges is unknown again.
        sim.apply(report(DeviceReport::Power {
            battery: Some(140),
            charging: None,
        }));
        let dev = &sim.store.snapshot().device;
        assert_eq!((dev.battery, dev.charging), (Some(100), None));
        // A blank board name is no board.
        sim.apply(report(DeviceReport::Board(Some("  ".into()))));
        assert!(sim.events.is_empty());
        assert_eq!(sim.store.snapshot().device.board, None);
    }

    #[test]
    fn going_live_remembers_the_deck_and_persists_it_once() {
        let mut sim = Sim::new();
        sim.apply(link(LinkEvent::Linked));
        sim.apply(report(DeviceReport::Transport(Some(Transport::Usb))));
        sim.apply(hello());
        sim.t += Duration::from_secs(1);
        let went_live = sim.store.ts(sim.t);
        sim.apply(link(LinkEvent::Live { sid: 314_159 }));
        assert_eq!(
            sim.kinds(),
            vec![EventKind::DeviceLink, EventKind::DeviceStatus],
            "the link first, then what it remembers"
        );
        let last = LastLinked {
            at: went_live,
            fw: Some(FW.into()),
            board: None,
            transport: Some(Transport::Usb),
        };
        assert_eq!(sim.store.snapshot().device.last, Some(last.clone()));
        assert_eq!(sim.effects, vec![Effect::Persist(StatePatch::LastLinked(last))]);
        assert_eq!(sim.events[1].data["last"]["fw"], FW);
        // A barrier's `Synced` and the `Live` after it are the same live link.
        sim.t += Duration::from_secs(1);
        for again in [LinkEvent::Synced { sid: 314_159 }, LinkEvent::Live { sid: 314_159 }] {
            sim.apply(link(again.clone()));
            assert!(sim.events.is_empty() && sim.effects.is_empty(), "{again:?}");
        }
    }

    #[test]
    fn a_board_or_transport_learned_after_going_live_is_remembered_with_the_live_time() {
        let mut sim = Sim::new();
        let went_live = live_on_usb(&mut sim);
        sim.t += Duration::from_secs(5);
        sim.apply(report(DeviceReport::Board(Some("Rev B".into()))));
        assert_eq!(sim.kinds(), vec![EventKind::DeviceStatus]);
        let last = LastLinked {
            at: went_live,
            fw: Some(FW.into()),
            board: Some("Rev B".into()),
            transport: Some(Transport::Usb),
        };
        assert_eq!(persisted_last(&sim), vec![last.clone()]);
        assert_eq!(sim.store.snapshot().device.last, Some(last));
        assert_eq!(sim.events[0].data["last"]["board"], "Rev B");
        sim.apply(report(DeviceReport::Transport(Some(Transport::Bluetooth))));
        assert_eq!(persisted_last(&sim)[0].transport, Some(Transport::Bluetooth));
        assert_eq!(persisted_last(&sim)[0].at, went_live);
        // The charge is not part of what is remembered.
        sim.apply(report(DeviceReport::Power {
            battery: Some(85),
            charging: Some(false),
        }));
        assert_eq!(sim.kinds(), vec![EventKind::DeviceStatus]);
        assert!(persisted_last(&sim).is_empty());
    }

    #[test]
    fn a_link_that_recovers_or_resyncs_keeps_the_moment_it_went_live() {
        let mut sim = Sim::new();
        let went_live = live_on_usb(&mut sim);
        sim.t += Duration::from_secs(15);
        sim.apply(link(LinkEvent::Degraded));
        sim.t += Duration::from_secs(5);
        sim.apply(link(LinkEvent::Live { sid: 314_159 }));
        assert_eq!(sim.kinds(), vec![EventKind::DeviceLink]);
        assert!(persisted_last(&sim).is_empty());
        sim.apply(link(LinkEvent::Overflow));
        sim.apply(link(LinkEvent::Synced { sid: 314_159 }));
        assert!(persisted_last(&sim).is_empty());
        assert_eq!(sim.store.snapshot().device.last.as_ref().unwrap().at, went_live);
    }

    #[test]
    fn a_drop_waits_for_its_down_which_clears_the_deck_and_stamps_when_it_was_lost() {
        let mut sim = Sim::new();
        live_on_usb(&mut sim);
        sim.apply(report(DeviceReport::Board(Some("Rev B".into()))));
        sim.apply(report(DeviceReport::Power {
            battery: Some(85),
            charging: Some(true),
        }));
        sim.t += Duration::from_secs(60);
        let lost = sim.store.ts(sim.t);
        let before = sim.store.snapshot().clone();
        sim.apply(drop_("write_stall"));
        assert!(sim.events.is_empty() && sim.effects.is_empty(), "the Down says it");
        assert_eq!(sim.store.snapshot(), &before, "and nothing changes before it does");
        sim.apply(link(LinkEvent::Down));
        assert_eq!(sim.kinds(), vec![EventKind::DeviceLink, EventKind::DeviceStatus]);
        let last = LastLinked {
            at: lost,
            fw: Some(FW.into()),
            board: Some("Rev B".into()),
            transport: Some(Transport::Usb),
        };
        let dev = &sim.store.snapshot().device;
        assert_eq!(
            (dev.transport, dev.battery, dev.charging, dev.board.as_deref()),
            (None, None, None, None)
        );
        assert_eq!(dev.fw, None, "the link's own firmware goes with it");
        assert_eq!(dev.last, Some(last.clone()), "and the remembered deck keeps it");
        assert_eq!(dev.drops, vec![LinkDrop { at: lost, why: "write_stall".into() }]);
        assert_eq!(persisted_last(&sim), vec![last]);
        assert_eq!(
            sim.events[1].data,
            json!({"transport": null, "battery": null, "charging": null, "board": null,
                   "last": {"at": lost, "fw": FW, "board": "Rev B", "transport": "usb"},
                   "drops": [{"at": lost, "why": "write_stall"}]})
        );
        // A link that is already gone ends only once.
        sim.apply(link(LinkEvent::Down));
        assert!(sim.events.is_empty() && sim.effects.is_empty());
    }

    #[test]
    fn closing_the_link_for_an_install_is_not_a_drop() {
        let mut sim = Sim::new();
        live_on_usb(&mut sim);
        sim.t += Duration::from_secs(60);
        sim.apply(drop_("firmware_update"));
        sim.apply(link(LinkEvent::Down));
        let dev = &sim.store.snapshot().device;
        assert!(dev.drops.is_empty());
        assert_eq!(dev.last.as_ref().map(|last| last.at), Some(sim.store.ts(sim.t)));
        assert_eq!(sim.kinds(), vec![EventKind::DeviceLink, EventKind::DeviceStatus]);
    }

    #[test]
    fn the_drops_reach_back_an_hour_and_keep_at_most_sixty() {
        /// A link opened and lost, the way the hid thread reports it.
        fn lose(sim: &mut Sim, why: &str) {
            sim.apply(link(LinkEvent::Linked));
            sim.apply(drop_(why));
            sim.apply(link(LinkEvent::Down));
        }
        let mut sim = Sim::new();
        let first = sim.store.ts(sim.t);
        for _ in 0..=LINK_DROPS_KEPT {
            lose(&mut sim, "read");
            sim.t += Duration::from_secs(30);
        }
        let drops = &sim.store.snapshot().device.drops;
        assert_eq!(drops.len(), LINK_DROPS_KEPT);
        assert_eq!(drops[0].at, first.plus(Duration::from_secs(30)), "the oldest went");
        assert!(drops.windows(2).all(|pair| pair[0].at < pair[1].at), "oldest first");
        // A drop exactly an hour after the newest still has it for company…
        let newest = drops[LINK_DROPS_KEPT - 1].at;
        sim.t += LINK_DROP_WINDOW - Duration::from_secs(30);
        lose(&mut sim, "removed");
        let ats: Vec<Ts> = sim.store.snapshot().device.drops.iter().map(|d| d.at).collect();
        assert_eq!(ats, vec![newest, newest.plus(LINK_DROP_WINDOW)]);
        // …and a millisecond later it is past the window.
        sim.t += Duration::from_millis(1);
        lose(&mut sim, "misses");
        let drops = &sim.store.snapshot().device.drops;
        let whys: Vec<&str> = drops.iter().map(|drop| drop.why.as_str()).collect();
        assert_eq!(whys, vec!["removed", "misses"]);
    }

    #[test]
    fn a_link_that_never_went_live_leaves_the_remembered_deck_as_it_was() {
        let yesterday = LastLinked {
            at: Ts(1_789_700_000_000),
            fw: Some("0.12.0-openwalkietalkie".into()),
            board: Some("Rev A".into()),
            transport: Some(Transport::Bluetooth),
        };
        let mut sim = Sim::with(|cfg| cfg.persisted.last_linked = Some(yesterday.clone()));
        assert_eq!(sim.store.snapshot().device.last, Some(yesterday.clone()), "seeded");
        // Opened on the cable, and it never answered: six misses end it.
        sim.apply(link(LinkEvent::Linked));
        sim.apply(report(DeviceReport::Transport(Some(Transport::Usb))));
        assert!(persisted_last(&sim).is_empty());
        sim.apply(drop_("misses"));
        sim.apply(link(LinkEvent::Down));
        assert!(persisted_last(&sim).is_empty());
        let dev = &sim.store.snapshot().device;
        assert_eq!(dev.last, Some(yesterday));
        assert_eq!(dev.transport, None);
        assert_eq!(dev.drops.len(), 1);
        assert_eq!(sim.kinds(), vec![EventKind::DeviceLink, EventKind::DeviceStatus]);
    }

    #[test]
    fn with_no_link_up_a_report_is_stale_and_a_drop_is_said_at_once() {
        let mut sim = Sim::new();
        for stale in [
            DeviceReport::Transport(Some(Transport::Usb)),
            DeviceReport::Power {
                battery: Some(50),
                charging: Some(true),
            },
            DeviceReport::Board(Some("Rev B".into())),
        ] {
            sim.apply(report(stale.clone()));
            assert!(sim.events.is_empty() && sim.effects.is_empty(), "{stale:?}");
        }
        assert_eq!(sim.store.snapshot().device, Sim::new().store.snapshot().device);
        sim.apply(link(LinkEvent::Down));
        assert!(sim.events.is_empty(), "no link, nothing ended");
        // Nothing is coming to say this one, so it says itself.
        sim.apply(drop_("removed"));
        assert_eq!(sim.kinds(), vec![EventKind::DeviceStatus]);
        assert_eq!(sim.events[0].data["drops"][0]["why"], "removed");
    }

    #[test]
    fn shutting_down_while_linked_stamps_when_the_deck_was_last_linked() {
        let mut sim = Sim::new();
        live_on_usb(&mut sim);
        sim.t += Duration::from_secs(3600);
        sim.apply(Command::Shutdown);
        let kinds = sim.kinds();
        let link_at = kinds.iter().position(|k| *k == EventKind::DeviceLink).unwrap();
        assert_eq!(kinds.get(link_at + 1), Some(&EventKind::DeviceStatus));
        assert_eq!(sim.events[link_at + 1].principal, Principal::Uid(0));
        let stamped = persisted_last(&sim);
        assert_eq!(stamped.len(), 1);
        assert_eq!(stamped[0].at, sim.store.ts(sim.t));
        assert_eq!(sim.store.snapshot().device.transport, None);
        // A service that stops with no deck says nothing about one.
        let mut idle = Sim::new();
        idle.apply(Command::Shutdown);
        assert_eq!(idle.kinds(), vec![EventKind::ServiceShutdown]);
    }
}
