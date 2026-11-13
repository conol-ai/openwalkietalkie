//! The core's side of the service's own looks for a release (R8): the
//! firmware and app update checks run in the daemon, so they go on with the
//! window closed and with the app not running at all.
//!
//! When to look is `firmware::Schedule`'s, and the look itself the
//! updater's (`firmware::Updater::check_scheduled`); the core decides on
//! every tick whether one may start now. It also answers the three firmware
//! methods with what the deck said of its own firmware in its hello
//! ([`Core::firmware_reply`]), which the updater cannot read: with that,
//! `firmware.status` is the one read that says what there is to update, for
//! the app and for `deckd` alike.
use super::Core;
use crate::firmware::{self, Schedule, Status};
use std::time::Instant;

impl Core {
    /// On every tick: start the service's own look for a release when the
    /// schedule says one is due ([`Schedule::due`]): a deck was linked in the
    /// last ten minutes (`Dormancy::deck_recent`), five minutes have passed
    /// since the start, six hours since the release feed last answered and
    /// an hour since a look could not reach it. Never in a service that
    /// never opens the deck (`--no-device`), and never with `[updates]
    /// check = false`.
    ///
    /// A look reads the deck on the cable from a thread of its own, beside
    /// the link, so none starts while someone talks (a held press, a
    /// transcription on its way, speech: [`Core::quiet`]) or while another
    /// update operation runs; the next tick asks again.
    pub(super) fn check_for_updates(&mut self, now: Instant) {
        if self.no_device || !self.config.updates.check {
            return;
        }
        let due = self.update_schedule.due(
            now,
            self.started,
            self.dormancy.deck_recent(),
            self.firmware.last_answered(),
            self.firmware.last_failed(),
        );
        if !due || !self.quiet() || self.firmware.busy() {
            return;
        }
        self.update_schedule.ran(now);
        let cache = self.paths.caches.join(firmware::CACHE_DIR);
        match self.firmware.check_scheduled(cache) {
            Ok(_) => tracing::info!(
                looks = self.update_schedule.looks(),
                "updates: looking for a release"
            ),
            Err(why) => tracing::warn!(%why, "updates: the look did not start"),
        }
    }

    /// The service's own looks for a release so far.
    pub fn update_schedule(&self) -> &Schedule {
        &self.update_schedule
    }

    /// A firmware status as `firmware.status`, `firmware.check` and
    /// `firmware.install` answer it: the updater's, with the firmware the
    /// deck named in its hello against the release (`Status::linked`). That
    /// is the deck linked now, else the last one linked here, as the Service
    /// page's firmware row names it.
    pub(crate) fn firmware_reply(&self, status: Status) -> serde_json::Value {
        let device = &self.store.snapshot().device;
        let fw = device
            .fw
            .as_deref()
            .or_else(|| device.last.as_ref()?.fw.as_deref());
        serde_json::to_value(status.with_linked(fw)).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use crate::config::Config;
    use crate::core::testkit::{ctx, detached, temp};
    use crate::core::Core;
    use crate::dormancy::{Dormancy, WINDOW};
    use crate::firmware::schedule::{GRACE, RETRY};
    use crate::hid::FromHid;
    use deck_model::{Command, DeviceLine, LinkEvent, PttCtx, Scope, VoiceIn};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn looks(c: &Core) -> u32 {
        c.update_schedule().looks()
    }

    fn minutes(m: u64) -> Duration {
        Duration::from_secs(m * 60)
    }

    /// The core with dormancy as `deckd run` starts it for a daemon that
    /// can have a deck: none linked yet.
    fn with_dormancy(name: &str) -> (Core, std::path::PathBuf, Instant) {
        let dir = temp(name);
        let (mut c, _ops, _lines, _debug) = detached(&dir);
        let t0 = Instant::now();
        let wall = c.store.ts(t0);
        c.set_dormancy(Dormancy::start(WINDOW, None, wall, t0), t0);
        (c, dir, t0)
    }

    /// R8 on a simulated clock: five minutes after the start, while a deck
    /// is around, the service looks once; a look that found nothing (the
    /// test core's updater never reaches out) is tried again an hour later;
    /// ten minutes after the deck leaves it stops looking, and it looks again
    /// once one is back.
    #[test]
    fn the_service_looks_for_updates_by_itself_while_a_deck_is_around() {
        let (mut c, dir, t0) = with_dormancy("updates-schedule");
        c.tick(t0 + minutes(30));
        assert_eq!(looks(&c), 0, "no deck was ever linked here");
        let t1 = t0 + minutes(31);
        c.on_from_hid(FromHid::Link(LinkEvent::Live { sid: 7 }), t1);
        c.tick(t1);
        assert_eq!(looks(&c), 1, "the grace is long over: at once");
        c.tick(t1 + Duration::from_millis(250));
        c.tick(t1 + minutes(59));
        assert_eq!(looks(&c), 1, "not on every tick");
        c.tick(t1 + RETRY);
        assert_eq!(looks(&c), 2, "an hour after a look that found nothing");
        // The deck leaves: within the ten minutes no look is due, and after
        // them none happens, however long.
        let gone = t1 + RETRY + minutes(1);
        c.on_from_hid(FromHid::Link(LinkEvent::Down), gone);
        for at in [minutes(9), minutes(11), RETRY * 2, RETRY * 30] {
            c.tick(gone + at);
        }
        assert_eq!(looks(&c), 2, "no deck for ten minutes: no look");
        // Back: due at once.
        let back = gone + RETRY * 31;
        c.on_from_hid(FromHid::Link(LinkEvent::Live { sid: 7 }), back);
        c.tick(back);
        assert_eq!(looks(&c), 3);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The first look waits five minutes after the start, deck or no deck.
    #[test]
    fn the_first_look_waits_for_the_grace() {
        let (mut c, dir, t0) = with_dormancy("updates-grace");
        c.on_from_hid(FromHid::Link(LinkEvent::Live { sid: 7 }), t0);
        c.tick(t0 + GRACE - Duration::from_secs(1));
        assert_eq!(looks(&c), 0);
        c.tick(t0 + GRACE);
        assert_eq!(looks(&c), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Never with `[updates] check = false`, which is hot; never in a service
    /// that never opens the deck; never in a core with no deck to wait for.
    #[test]
    fn the_switch_no_device_and_no_dormancy_each_keep_it_from_looking() {
        let (mut c, dir, t0) = with_dormancy("updates-off");
        c.on_from_hid(FromHid::Link(LinkEvent::Live { sid: 7 }), t0);
        let mut off = Config::default();
        off.updates.check = false;
        c.set_config(Arc::new(off));
        c.tick(t0 + minutes(10));
        assert_eq!(looks(&c), 0, "switched off");
        c.set_config(Arc::new(Config::default()));
        c.tick(t0 + minutes(10));
        assert_eq!(looks(&c), 1, "switched on again: at the next tick");
        std::fs::remove_dir_all(dir).unwrap();

        let (mut c, dir, t0) = with_dormancy("updates-no-device");
        c.no_device = true;
        c.on_from_hid(FromHid::Link(LinkEvent::Live { sid: 7 }), t0);
        c.tick(t0 + minutes(10));
        assert_eq!(looks(&c), 0, "--no-device");
        std::fs::remove_dir_all(dir).unwrap();

        let dir = temp("updates-no-dormancy");
        let (mut c, _ops, _lines, _debug) = detached(&dir);
        let t0 = Instant::now();
        c.on_from_hid(FromHid::Link(LinkEvent::Live { sid: 7 }), t0);
        c.tick(t0 + minutes(10));
        assert_eq!(
            looks(&c),
            0,
            "a core no `run` started has no deck to wait for"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A look reads the deck on the cable beside the link: not while a press
    /// is held or its words are on their way. It starts once they are.
    #[test]
    fn no_look_while_someone_talks() {
        let (mut c, dir, t0) = with_dormancy("updates-quiet");
        let (audio, _commands, _events) = crate::audio::AudioHandle::detached();
        c.set_audio(audio);
        c.on_from_hid(FromHid::Link(LinkEvent::Live { sid: 7 }), t0);
        let ptt = |act| {
            Command::Device(DeviceLine::Ptt {
                act,
                slot: 1,
                ctx: PttCtx::Talk,
                card: None,
                abort: false,
                press: None,
            })
        };
        let at = t0 + minutes(10);
        c.step(ptt(1), at);
        c.tick(at);
        assert_eq!(looks(&c), 0, "a press is held");
        c.step(ptt(0), at);
        c.tick(at);
        assert_eq!(looks(&c), 0, "its words are on their way");
        c.step(
            Command::Voice(VoiceIn::Final {
                text: "Done.".into(),
            }),
            at,
        );
        c.tick(at);
        assert_eq!(looks(&c), 1, "quiet again");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// `firmware.status` says what the deck named in its hello against the
    /// release: the deck linked now, and once it is gone the last one linked
    /// here. A test core's updater has found no release, so nothing is
    /// newer.
    #[test]
    fn the_firmware_reply_names_the_hellos_firmware() {
        let (mut c, dir, t0) = with_dormancy("updates-linked");
        let status = |c: &mut Core| {
            c.call(
                ctx(2, &[Scope::Observe]),
                "firmware.status",
                serde_json::json!({}),
                t0,
            )
            .result
            .unwrap()
        };
        assert!(status(&mut c).get("linked").is_none(), "no deck has said");
        c.on_from_hid(FromHid::Link(LinkEvent::Linked), t0);
        c.on_device_line(
            DeviceLine::Hello {
                v: 1,
                fw: "0.13.0-openwalkietalkie".into(),
                sid: 7,
                boot: false,
                rx: 1024,
                agents: None,
                lang: None,
                lang_rev: None,
                faces: vec![],
            },
            t0,
        );
        c.on_from_hid(FromHid::Link(LinkEvent::Live { sid: 7 }), t0);
        let linked = serde_json::json!({"fw": "0.13.0-openwalkietalkie", "available": false});
        assert_eq!(status(&mut c)["linked"], linked);
        c.on_from_hid(FromHid::Link(LinkEvent::Down), t0);
        assert_eq!(c.store.snapshot().device.fw, None);
        assert_eq!(
            status(&mut c)["linked"],
            linked,
            "the last deck linked here"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
