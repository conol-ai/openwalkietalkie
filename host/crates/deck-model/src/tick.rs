//! `Command::Tick`: every deadline is re-read from the state, so a stale
//! timer is harmless and one tick settles everything that is due.
use crate::{AgentId, CardStatus, Deliver, Effect, Release, Store};
use std::time::Duration;

impl Store {
    pub(crate) fn on_tick(&mut self) {
        self.tick_session_controls();
        let now = self.now;
        let grace = Duration::from_secs(u64::from(self.cfg.grace_s));
        let ids: Vec<AgentId> = self.snapshot.agents.keys().cloned().collect();
        for id in ids {
            let Some(aux) = self.aux.get(&id) else {
                continue;
            };
            if aux.lease_at.is_some_and(|at| at <= now) && aux.orphaned_at.is_none() {
                self.orphan(&id, "lease");
            }
            if self
                .aux
                .get(&id)
                .and_then(|a| a.orphaned_at)
                .is_some_and(|at| at + grace <= now)
            {
                self.remove_agent(&id, "grace");
                continue;
            }
            let Some(aux) = self.aux.get(&id) else {
                continue;
            };
            let (deliver_at, hold_at, expire_at) = (aux.deliver_at, aux.hold_at, aux.expire_at);
            let Some(card) = self.snapshot.agents.get(&id).and_then(|a| a.card.as_ref()) else {
                continue;
            };
            let (status, deliver, cid) = (card.status.clone(), card.deliver, card.id);
            if deliver_at.is_some_and(|at| at <= now)
                && matches!(status, CardStatus::Answering { .. } | CardStatus::Delivered)
            {
                self.fail_card(&id, "timeout");
                continue;
            }
            if hold_at.is_some_and(|at| at <= now)
                && deliver == Deliver::Hold
                && matches!(status, CardStatus::Pending | CardStatus::Snoozed)
            {
                if self.snapshot.agents.get(&id).is_some_and(|a| self.is_muted(a)) {
                    // Held while its agent was muted, with a deadline already
                    // due (`card_set`): the core has parked it by now, so it
                    // can go back to its tool.
                    self.release_to_tool(&id);
                } else {
                    self.effects.push(Effect::ReleaseHold {
                        agent: id.clone(),
                        card: cid,
                        reason: Release::Expiry,
                    });
                    self.degrade_card(&id, "expiry");
                }
                continue;
            }
            if expire_at.is_some_and(|at| at <= now)
                && matches!(
                    status,
                    CardStatus::Pending | CardStatus::Snoozed | CardStatus::Failed { .. }
                )
            {
                self.expire_card(&id, "ttl");
            }
        }
        // The cap belongs to the capture that is open and to nothing else:
        // `ptt_cap_at` is cleared with the live session, so a due cap always
        // means a live session — and `abort_live` is a no-op if it does not.
        if self.ptt_cap_at.is_some_and(|at| at <= now) {
            self.abort_live("cap");
        }
        // §7.4's two PTT deadlines, in the order they can become due:
        // `Arming` belongs to a live capture, `Finalizing` to one that has
        // already been released, so at most one of them fires per session.
        if self.arming_at.is_some_and(|at| at <= now) {
            self.arming_timed_out();
        }
        if self.finalizing_at.is_some_and(|at| at <= now) {
            self.finalize_timed_out();
        }
        if self.usage_refresh_at.is_some_and(|at| at <= now) {
            self.usage_refresh_at = None;
            self.resend_usage();
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::testkit::{Sim, UID};
    use crate::{CardKind, Command, DeviceLine, EventKind, SessionId, TimerKey};
    use std::time::{Duration, Instant};

    #[test]
    fn a_tick_with_nothing_due_is_silent_and_stale_timers_are_harmless() {
        let mut sim = Sim::new();
        let id = sim.register(UID, SessionId(1), "bot", None);
        sim.set_card(&id, CardKind::Q, crate::Deliver::Poll, vec!["a"]);
        sim.advance(Duration::from_secs(1));
        assert!(sim.events.is_empty() && sim.effects.is_empty());
        // A timer for a card that is gone: nothing happens on its tick.
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 1,
            card: 1,
            choice: 0,
        }));
        let due = sim
            .effects
            .iter()
            .find_map(|e| match e {
                crate::Effect::Timer {
                    key: TimerKey::DeliveryTimeout(_),
                    at,
                } => Some(*at),
                _ => None,
            })
            .unwrap();
        sim.apply(Command::Delivered {
            agent: id.clone(),
            card: crate::CardId(1),
            ok: true,
        });
        sim.t = due;
        sim.apply(Command::Tick(due));
        assert!(sim.events.is_empty() && sim.effects.is_empty());
        // Time never goes backwards for the store: a tick in the past is fine.
        let (events, _) = sim.store.apply(
            Command::Tick(Instant::now()),
            sim.t - Duration::from_secs(10),
        );
        assert!(events.is_empty());
    }

    #[test]
    fn one_late_tick_settles_every_due_deadline() {
        let mut sim = Sim::new();
        let a = sim.register(UID, SessionId(1), "a", None);
        let b = sim.register(UID, SessionId(2), "b", None);
        sim.set_card(&a, CardKind::R, crate::Deliver::Poll, vec!["x"]);
        sim.set_card(&b, CardKind::Q, crate::Deliver::Poll, vec!["y"]);
        sim.apply(Command::Device(DeviceLine::Answer {
            slot: 2,
            card: 1,
            choice: 0,
        }));
        sim.advance(Duration::from_secs(1000));
        let kinds = sim.kinds();
        assert!(kinds.contains(&EventKind::CardFailed), "{kinds:?}");
        assert!(kinds.contains(&EventKind::CardExpired));
        assert_eq!(
            kinds
                .iter()
                .filter(|k| **k == EventKind::AgentChanged)
                .count(),
            2,
            "both leases expired: {kinds:?}"
        );
        // The grace starts at the orphaning tick, never earlier.
        assert!(!kinds.contains(&EventKind::AgentRemoved));
        sim.advance(Duration::from_secs(299));
        assert!(sim.events.is_empty());
        sim.advance(Duration::from_secs(2));
        assert_eq!(
            sim.kinds()
                .iter()
                .filter(|k| **k == EventKind::AgentRemoved)
                .count(),
            2
        );
        assert!(sim.store.snapshot().agents.is_empty());
    }
}
