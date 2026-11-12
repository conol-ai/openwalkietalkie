//! What needs the owner's attention, as one ordered list. The Deck page's
//! cards, the menu's **Show Link Problem…** and the status item's care icon
//! all read this one list, so the three cannot disagree about whether
//! something is wrong.
//!
//! It is a pure function, like [`crate::menu`]'s model: the mirror, whether
//! the service is answering, the time, and the two facts the mirror does not
//! carry ([`Extras`]), which the pages that read them pass in. Nothing here
//! asks the service anything.
use crate::admin::port_clash;
use deck_model::{DeviceState, Link, Snapshot, Ts, LINK_DROP_WINDOW};

/// From this many link drops in the last hour the link is a problem worth a
/// card. Fewer is a cable knocked once or a walk out of range, which the
/// link recovers from on its own.
pub const ATTENTION_DROPS: u32 = 3;

/// One thing that needs attention, in the order [`attention`] lists them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attention {
    /// The link dropped `count` times in the last hour, at least
    /// [`ATTENTION_DROPS`].
    LinkDrops { count: u32 },
    /// The deck is linked and not answering (`link: degraded`).
    NotAnswering,
    /// The service says it is degraded, and why, in its own word. Never one
    /// of the two words about its TCP port ([`crate::admin::port_clash`]):
    /// those are the local API's, said on the Service page's Local API row,
    /// and nothing the deck or a session needs goes through that port.
    ServiceDegraded { reason: String },
    /// Claude sessions that still run a helper the service now refuses.
    StaleHelpers { count: u32 },
    /// Deck firmware `version` is available.
    FirmwareUpdate { version: String },
}

impl Attention {
    /// Whether this is about the link itself: what **Show Link Problem…**
    /// and the Deck page's link details are there for.
    pub const fn is_link_problem(&self) -> bool {
        matches!(self, Attention::LinkDrops { .. } | Attention::NotAnswering)
    }
}

/// What the mirror does not carry and [`attention`] needs: the pages that
/// read these from the service pass them in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Extras {
    /// Sessions still on an old Claude Channels helper (Integrations).
    pub stale_helpers: u32,
    /// A deck firmware version the service offers and the deck does not
    /// run yet (Service).
    pub firmware: Option<String>,
}

/// The link drops of the hour up to `now`, a drop exactly an hour old
/// included, as the service's own window counts it. The service prunes its
/// list only when it adds a drop, so older ones can still be in the mirror
/// and are not counted. A drop stamped a moment after `now` (the two clocks
/// differ a little) is recent, and counts.
pub fn drops_last_hour(device: &DeviceState, now: Ts) -> u32 {
    let window = LINK_DROP_WINDOW.as_millis() as u64;
    let recent = device
        .drops
        .iter()
        .filter(|drop| now.millis().saturating_sub(drop.at.millis()) <= window)
        .count();
    u32::try_from(recent).unwrap_or(u32::MAX)
}

/// Everything that needs attention now, in the order the Deck page shows
/// it: the link's drops, a link that is not answering, the service, old
/// helpers, then a firmware update.
///
/// Empty while the service is not answering: the mirror is then the last
/// one it sent, and a warning read off it would be about a service that is
/// not there. The not-running state says what there is to say.
pub fn attention(snapshot: &Snapshot, connected: bool, now: Ts, extras: &Extras) -> Vec<Attention> {
    if !connected {
        return Vec::new();
    }
    let mut items = Vec::new();
    let drops = drops_last_hour(&snapshot.device, now);
    if drops >= ATTENTION_DROPS {
        items.push(Attention::LinkDrops { count: drops });
    }
    if snapshot.device.link == Link::Degraded {
        items.push(Attention::NotAnswering);
    }
    // A service on its way out is not degraded: it is going, and the
    // not-running state that follows says so. A port another program holds
    // closes the local API and nothing else, so it is not this list's
    // either: the Service page's Local API row says it.
    if let Some(reason) = snapshot
        .service
        .degraded
        .as_ref()
        .filter(|reason| *reason != crate::host_state::SHUTTING_DOWN && !port_clash(reason))
    {
        items.push(Attention::ServiceDegraded {
            reason: reason.clone(),
        });
    }
    if extras.stale_helpers > 0 {
        items.push(Attention::StaleHelpers {
            count: extras.stale_helpers,
        });
    }
    if let Some(version) = &extras.firmware {
        items.push(Attention::FirmwareUpdate {
            version: version.clone(),
        });
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use deck_model::LinkDrop;
    use std::time::Duration;

    /// 2026-11-11T10:02:00.000Z, the harness's pinned minute.
    const NOW: Ts = Ts(1_794_391_320_000);

    fn drop_at(ago: Duration) -> LinkDrop {
        LinkDrop {
            at: Ts(NOW.millis() - ago.as_millis() as u64),
            why: "read".into(),
        }
    }

    fn linked() -> Snapshot {
        let mut snapshot = Snapshot::default();
        snapshot.device.link = Link::Connected;
        snapshot
    }

    #[test]
    fn the_last_hour_holds_a_drop_exactly_an_hour_old_and_nothing_older() {
        let mut device = DeviceState::default();
        assert_eq!(drops_last_hour(&device, NOW), 0);
        device.drops = vec![
            // Older than the window: still in the mirror, never counted.
            drop_at(LINK_DROP_WINDOW + Duration::from_millis(1)),
            drop_at(LINK_DROP_WINDOW),
            drop_at(Duration::from_secs(60)),
            drop_at(Duration::ZERO),
        ];
        assert_eq!(drops_last_hour(&device, NOW), 3);
        // A millisecond later the oldest of the three has left the window.
        assert_eq!(drops_last_hour(&device, NOW.plus(Duration::from_millis(1))), 2);
        // A drop the service stamped just after the app's `now` is recent.
        device.drops.push(LinkDrop {
            at: NOW.plus(Duration::from_millis(400)),
            why: "write_stall".into(),
        });
        assert_eq!(drops_last_hour(&device, NOW), 4);
    }

    #[test]
    fn each_item_is_raised_by_its_own_fact_and_by_nothing_else() {
        let quiet = linked();
        assert_eq!(attention(&quiet, true, NOW, &Extras::default()), vec![]);

        let mut dropping = linked();
        dropping.device.drops = (0..2).map(|m| drop_at(Duration::from_secs(60 * m))).collect();
        assert_eq!(
            attention(&dropping, true, NOW, &Extras::default()),
            vec![],
            "two drops in an hour are not a problem yet"
        );
        dropping.device.drops.push(drop_at(Duration::from_secs(600)));
        assert_eq!(
            attention(&dropping, true, NOW, &Extras::default()),
            vec![Attention::LinkDrops { count: 3 }]
        );

        let mut silent = linked();
        silent.device.link = Link::Degraded;
        assert_eq!(
            attention(&silent, true, NOW, &Extras::default()),
            vec![Attention::NotAnswering]
        );

        let mut degraded = linked();
        degraded.service.degraded = Some("disk_full".into());
        assert_eq!(
            attention(&degraded, true, NOW, &Extras::default()),
            vec![Attention::ServiceDegraded {
                reason: "disk_full".into()
            }]
        );
        // A service that is shutting down is not one that needs care.
        degraded.service.degraded = Some(crate::host_state::SHUTTING_DOWN.into());
        assert_eq!(attention(&degraded, true, NOW, &Extras::default()), vec![]);

        let helpers = Extras {
            stale_helpers: 2,
            ..Extras::default()
        };
        assert_eq!(
            attention(&quiet, true, NOW, &helpers),
            vec![Attention::StaleHelpers { count: 2 }]
        );
        let firmware = Extras {
            firmware: Some("0.14.0".into()),
            ..Extras::default()
        };
        assert_eq!(
            attention(&quiet, true, NOW, &firmware),
            vec![Attention::FirmwareUpdate {
                version: "0.14.0".into()
            }]
        );
    }

    #[test]
    fn the_items_come_in_the_deck_pages_order_and_only_two_are_link_problems() {
        let mut everything = linked();
        everything.device.link = Link::Degraded;
        everything.device.drops = (0..41).map(|m| drop_at(Duration::from_secs(60 * m))).collect();
        everything.service.degraded = Some("disk_full".into());
        let extras = Extras {
            stale_helpers: 1,
            firmware: Some("0.14.0".into()),
        };
        let items = attention(&everything, true, NOW, &extras);
        assert_eq!(
            items,
            vec![
                Attention::LinkDrops { count: 41 },
                Attention::NotAnswering,
                Attention::ServiceDegraded {
                    reason: "disk_full".into()
                },
                Attention::StaleHelpers { count: 1 },
                Attention::FirmwareUpdate {
                    version: "0.14.0".into()
                },
            ]
        );
        let link: Vec<bool> = items.iter().map(Attention::is_link_problem).collect();
        assert_eq!(link, vec![true, true, false, false, false]);
    }

    #[test]
    fn a_service_that_is_not_answering_raises_nothing_from_its_last_mirror() {
        let mut stale = linked();
        stale.device.link = Link::Degraded;
        stale.device.drops = (0..5).map(|m| drop_at(Duration::from_secs(60 * m))).collect();
        stale.service.degraded = Some("disk_full".into());
        let extras = Extras {
            stale_helpers: 3,
            firmware: Some("0.14.0".into()),
        };
        assert_eq!(attention(&stale, false, NOW, &extras), vec![]);
        // The drops of a deck that is gone still count while the service
        // answers: the link was a problem, and the details still say how.
        stale.device.link = Link::Offline;
        assert_eq!(
            attention(&stale, true, NOW, &Extras::default()),
            vec![
                Attention::LinkDrops { count: 5 },
                Attention::ServiceDegraded {
                    reason: "disk_full".into()
                },
            ]
        );
    }

    #[test]
    fn another_program_on_the_local_apis_port_is_never_an_attention_item() {
        // Linked, linked and silent, gone: a port another program holds
        // (and one it answers on) closes the local API alone, so it raises
        // nothing, and beside it the list is what it would be without it.
        for link in [Link::Connected, Link::Degraded, Link::Offline] {
            for reason in [crate::admin::PORT_IN_USE, crate::admin::HIJACK_RISK] {
                let mut clash = linked();
                clash.device.link = link;
                clash.service.degraded = Some(reason.into());
                let mut without = clash.clone();
                without.service.degraded = None;
                assert_eq!(
                    attention(&clash, true, NOW, &Extras::default()),
                    attention(&without, true, NOW, &Extras::default()),
                    "{link:?} {reason}"
                );
                assert!(!attention(&clash, true, NOW, &Extras::default())
                    .iter()
                    .any(|item| matches!(item, Attention::ServiceDegraded { .. })));
            }
        }
    }
}
