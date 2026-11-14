//! The session state machine (design §6.3), driven by the hid thread with a
//! simulated clock: hello every 5 s, 2 s reply timeout, 3 misses → Degraded
//! (handle kept), 6 misses → reopen; a reply with a foreign sid → Syncing.
use deck_model::SessionState;
use std::time::{Duration, Instant};

pub const HELLO_EVERY: Duration = Duration::from_secs(5);
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(2);
pub const MISSES_DEGRADED: u32 = 3;
pub const MISSES_REOPEN: u32 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// The device was opened.
    LinkUp,
    /// Read/write error or removal.
    LinkDown,
    /// The device answered a hello.
    HelloReply {
        sid: u32,
        boot: bool,
    },
    /// The barrier's `synced` was written.
    BarrierDone,
    Tick,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    SendHello,
    /// Write a barrier from the view.
    StartSync {
        boot: bool,
    },
    /// Drop the handle and re-enumerate.
    Reopen,
    /// Report the new state to the core.
    Report(SessionState),
}

#[derive(Debug)]
pub struct Session {
    sid: u32,
    hello_every: Duration,
    state: SessionState,
    misses: u32,
    /// When the pending hello was sent, if a reply is awaited.
    sent_at: Option<Instant>,
    last_hello: Option<Instant>,
}

impl Session {
    pub fn new(sid: u32, hello_every: Duration) -> Self {
        Session {
            sid,
            hello_every,
            state: SessionState::Absent,
            misses: 0,
            sent_at: None,
            last_hello: None,
        }
    }

    pub fn sid(&self) -> u32 {
        self.sid
    }

    /// Another deck is in the hand (`hid::Link::identified`): its replies
    /// are compared with `sid` from now on.
    pub fn set_sid(&mut self, sid: u32) {
        self.sid = sid;
    }

    pub fn state(&self) -> SessionState {
        self.state
    }

    pub fn misses(&self) -> u32 {
        self.misses
    }

    /// Advance the machine; the actions are executed by the caller in order.
    pub fn step(&mut self, input: Input, now: Instant) -> Vec<Action> {
        let mut actions = Vec::new();
        let before = self.state;
        match input {
            Input::LinkUp => {
                self.misses = 0;
                self.state = SessionState::AwaitHello;
                self.hello(now, &mut actions);
            }
            Input::LinkDown => {
                self.state = SessionState::Absent;
                self.sent_at = None;
                self.last_hello = None;
                self.misses = 0;
            }
            Input::HelloReply { sid, boot } => {
                if self.state == SessionState::Absent {
                    return actions;
                }
                self.sent_at = None;
                self.misses = 0;
                if sid == self.sid && sid != 0 {
                    if self.state != SessionState::Syncing {
                        self.state = SessionState::Live;
                    }
                } else if self.state != SessionState::Syncing {
                    self.state = SessionState::Syncing;
                    actions.push(Action::StartSync { boot });
                }
            }
            Input::BarrierDone => {
                if self.state == SessionState::Syncing {
                    self.state = SessionState::Live;
                }
            }
            Input::Tick => {
                if self.state == SessionState::Absent {
                    return actions;
                }
                if let Some(sent) = self.sent_at {
                    if now.duration_since(sent) >= REPLY_TIMEOUT {
                        self.misses += 1;
                        self.sent_at = None;
                        if self.misses >= MISSES_REOPEN {
                            self.state = SessionState::Absent;
                            actions.push(Action::Reopen);
                        } else if self.misses >= MISSES_DEGRADED {
                            self.state = SessionState::Degraded;
                        }
                        if self.state == SessionState::AwaitHello {
                            // Retry at once while waiting for the first reply.
                            self.hello(now, &mut actions);
                        }
                    }
                }
                if self.state != SessionState::Absent
                    && self.sent_at.is_none()
                    && self
                        .last_hello
                        .is_none_or(|t| now.duration_since(t) >= self.hello_every)
                {
                    self.hello(now, &mut actions);
                }
            }
        }
        if self.state != before {
            actions.push(Action::Report(self.state));
        }
        actions
    }

    fn hello(&mut self, now: Instant, actions: &mut Vec<Action>) {
        self.sent_at = Some(now);
        self.last_hello = Some(now);
        actions.push(Action::SendHello);
    }
}

/// `"HH:MM"` in the local zone for `hello.hm`; `None` when the zone is unknown.
#[cfg(windows)]
pub fn local_hm(now: std::time::SystemTime) -> Option<String> {
    let local: chrono::DateTime<chrono::Local> = now.into();
    Some(local.format("%H:%M").to_string())
}

/// `"HH:MM"` in the local zone for `hello.hm`; `None` when the zone is unknown.
#[cfg(unix)]
pub fn local_hm(now: std::time::SystemTime) -> Option<String> {
    let secs = now.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let t = libc::time_t::try_from(secs).ok()?;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: localtime_r writes only into `tm`, which we own.
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return None;
    }
    Some(format!("{:02}:{:02}", tm.tm_hour, tm.tm_min))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> (Session, Instant) {
        (Session::new(314_159, HELLO_EVERY), Instant::now())
    }

    #[test]
    fn link_up_hellos_and_a_matching_reply_goes_live() {
        let (mut s, t0) = fresh();
        assert_eq!(
            s.step(Input::LinkUp, t0),
            vec![Action::SendHello, Action::Report(SessionState::AwaitHello)]
        );
        assert_eq!(s.step(Input::Tick, t0 + Duration::from_millis(500)), vec![]);
        assert_eq!(
            s.step(
                Input::HelloReply {
                    sid: 314_159,
                    boot: false
                },
                t0 + Duration::from_millis(600)
            ),
            vec![Action::Report(SessionState::Live)]
        );
        // Heartbeat at 5 s, not before.
        assert_eq!(
            s.step(Input::Tick, t0 + Duration::from_millis(4999)),
            vec![]
        );
        assert_eq!(
            s.step(Input::Tick, t0 + Duration::from_millis(5000)),
            vec![Action::SendHello]
        );
        assert_eq!(s.state(), SessionState::Live);
    }

    #[test]
    fn a_foreign_or_zero_sid_starts_a_sync_and_synced_returns_to_live() {
        let (mut s, t0) = fresh();
        s.step(Input::LinkUp, t0);
        assert_eq!(
            s.step(Input::HelloReply { sid: 0, boot: true }, t0),
            vec![
                Action::StartSync { boot: true },
                Action::Report(SessionState::Syncing)
            ]
        );
        // A second reply during the sync does not restart it.
        assert_eq!(s.step(Input::HelloReply { sid: 0, boot: true }, t0), vec![]);
        assert_eq!(
            s.step(Input::BarrierDone, t0),
            vec![Action::Report(SessionState::Live)]
        );
        // Device rebooted: a foreign sid while live → sync again.
        assert_eq!(
            s.step(
                Input::HelloReply {
                    sid: 99,
                    boot: false
                },
                t0
            ),
            vec![
                Action::StartSync { boot: false },
                Action::Report(SessionState::Syncing)
            ]
        );
    }

    #[test]
    fn misses_degrade_at_three_and_reopen_at_six() {
        let (mut s, t0) = fresh();
        s.step(Input::LinkUp, t0);
        s.step(
            Input::HelloReply {
                sid: 314_159,
                boot: false,
            },
            t0,
        );
        let mut t = t0;
        let mut states = Vec::new();
        for _ in 0..6 {
            t += Duration::from_secs(5);
            assert_eq!(
                s.step(Input::Tick, t),
                vec![Action::SendHello],
                "hello every 5 s"
            );
            t += Duration::from_secs(2);
            let actions = s.step(Input::Tick, t);
            states.push((s.misses(), s.state(), actions));
        }
        assert_eq!(states[0].0, 1);
        assert_eq!(states[1].1, SessionState::Live);
        assert_eq!(states[2].1, SessionState::Degraded);
        assert_eq!(states[2].2, vec![Action::Report(SessionState::Degraded)]);
        assert_eq!(states[4].1, SessionState::Degraded);
        assert_eq!(states[5].0, 6);
        assert_eq!(
            states[5].2,
            vec![Action::Reopen, Action::Report(SessionState::Absent)]
        );
        // A late reply while degraded goes straight back to live.
        let (mut s, t0) = fresh();
        s.step(Input::LinkUp, t0);
        s.step(
            Input::HelloReply {
                sid: 314_159,
                boot: false,
            },
            t0,
        );
        let mut t = t0;
        for _ in 0..3 {
            t += Duration::from_secs(5);
            s.step(Input::Tick, t);
            t += Duration::from_secs(2);
            s.step(Input::Tick, t);
        }
        assert_eq!(s.state(), SessionState::Degraded);
        assert_eq!(
            s.step(
                Input::HelloReply {
                    sid: 314_159,
                    boot: false
                },
                t
            ),
            vec![Action::Report(SessionState::Live)]
        );
        assert_eq!(s.misses(), 0);
    }

    #[test]
    fn await_hello_retries_every_two_seconds() {
        let (mut s, t0) = fresh();
        s.step(Input::LinkUp, t0);
        assert_eq!(
            s.step(Input::Tick, t0 + Duration::from_secs(2)),
            vec![Action::SendHello]
        );
        assert_eq!(s.misses(), 1);
        assert_eq!(
            s.step(Input::Tick, t0 + Duration::from_secs(4)),
            vec![Action::SendHello]
        );
        let actions = s.step(Input::Tick, t0 + Duration::from_secs(6));
        assert_eq!(s.state(), SessionState::Degraded);
        assert!(actions.contains(&Action::Report(SessionState::Degraded)));
        s.step(Input::LinkDown, t0 + Duration::from_secs(7));
        assert_eq!(s.state(), SessionState::Absent);
        assert_eq!(s.step(Input::Tick, t0 + Duration::from_secs(20)), vec![]);
        assert_eq!(
            s.step(
                Input::HelloReply {
                    sid: 1,
                    boot: false
                },
                t0
            ),
            vec![]
        );
        assert_eq!(s.sid(), 314_159);
    }

    #[test]
    fn local_hm_is_a_clock() {
        let hm = local_hm(std::time::SystemTime::now()).unwrap();
        assert_eq!(hm.len(), 5);
        assert_eq!(&hm[2..3], ":");
        assert!(deck_proto::encode(&deck_proto::HostLine::Hello {
            sid: 1,
            hm: Some(hm)
        })
        .is_ok());
    }
}
