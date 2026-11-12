//! Channel 2 from the `deckd-hid` thread (D1): the deck's own system RPC,
//! asked for what nothing on channel 5 says — its board (`sys.version`,
//! once per link) and its charge (`device.status`, every [`STATUS_EVERY`])
//! — while the session is live.
//!
//! [`StatusPoll`] is the whole exchange without the I/O: which request is
//! due, the deck's replies read back into `DeviceReport`s for the core, the
//! timeouts on the hid loop's clock. `Link` writes a request beside the deck
//! lines, outside the outbox's pace and barrier as a voice frame is, and
//! only when the outbox has nothing due (`Link::pump_writer`).
//!
//! The channel is shared, and each risk of speaking on it (wire map §3.3)
//! is met here:
//!
//! - Other programs ask the same deck on channel 2 from handles of their
//!   own (`firmware.check` asks `sys.version` as 47201, `flash-usb.py` and
//!   the bench tools likewise), and macOS hands every handle every reply.
//!   Ours carry ids nobody else uses, a reply with another id is ignored,
//!   and seeing one holds our requests back for [`FOREIGN_QUIET`]: the deck
//!   reassembles one request per link, and a request of ours landing
//!   between two reports of someone else's would reset theirs. The
//!   updater's own reader passes over our replies, whole or cut
//!   (`firmware::transport`).
//! - A request is a literal that fits one report, so it never fragments,
//!   and it starts `{"method"` whatever the build (`json!` keeps that key
//!   first only where `serde_json/preserve_order` is on), the prefix the
//!   deck resets a stale partial request on. One report every 30 s never
//!   makes the burst that asks the deck for its fast Bluetooth link.
//! - No reply is a normal outcome (a deck in recovery, a link that drops):
//!   one request is in flight at a time, it is given up after
//!   [`STATUS_TIMEOUT`], and the next one still goes on time.
//! - A reply is read by its line feed, never by its report: the deck cuts a
//!   reply every 61 bytes, and a 61-byte reply sends its line feed alone. A
//!   reply cut short (a timeout, a dropped link) is dropped, not glued to
//!   the next one.
//! - `-32601` for a method stops asking it on this link; a line that cannot
//!   be read is counted and ignored.
//! - Nothing is asked before the session is live, nor while the device is
//!   closed for an install, and `[device] status_poll_s = 0` asks nothing.
use deck_model::DeviceReport;
use deck_proto::{PAYLOAD_MAX, REPORT_ID, REPORT_LEN};
use serde_json::Value;
use std::time::{Duration, Instant};

/// The deck's system RPC channel.
pub const CHANNEL: u8 = 2;
/// How often the charge is asked while the session is live, unless
/// `[device] status_poll_s` says otherwise. The deck samples its battery
/// every 15 s and moves its estimate 5 % at a time, so asking more often
/// buys nothing.
pub const STATUS_EVERY: Duration = Duration::from_secs(30);
/// The floor under `status_poll_s`: never more often than the hello.
pub const STATUS_EVERY_MIN: Duration = Duration::from_secs(5);
/// A request unanswered this long is given up (the updater waits as long).
pub const STATUS_TIMEOUT: Duration = Duration::from_secs(2);
/// After another program's reply on channel 2, nothing is asked for this
/// long: that program is in the middle of a conversation with the deck.
pub const FOREIGN_QUIET: Duration = Duration::from_secs(10);
/// The `id` of our `device.status` request. The updater asks as 47201.
pub const STATUS_ID: u64 = 47301;
/// The `id` of our `sys.version` request.
pub const VERSION_ID: u64 = 47302;
/// The requests, byte for byte.
const STATUS_REQUEST: &[u8] = b"{\"method\":\"device.status\",\"id\":47301}\n";
const VERSION_REQUEST: &[u8] = b"{\"method\":\"sys.version\",\"id\":47302}\n";
// Each fits one report.
const _: () = assert!(STATUS_REQUEST.len() <= PAYLOAD_MAX && VERSION_REQUEST.len() <= PAYLOAD_MAX);
/// The deck's replies are at most 640 bytes; a line longer than this is
/// nobody's reply.
const LINE_MAX: usize = 1024;
/// JSON-RPC's "Method not found": the deck's answer to a method it lacks.
const METHOD_NOT_FOUND: i64 = -32601;

/// One of our two requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ask {
    /// `sys.version`, for the board.
    Version,
    /// `device.status`, for the charge.
    Status,
}

impl Ask {
    fn of(id: u64) -> Option<Ask> {
        match id {
            VERSION_ID => Some(Ask::Version),
            STATUS_ID => Some(Ask::Status),
            _ => None,
        }
    }

    fn method(self) -> &'static str {
        match self {
            Ask::Version => "sys.version",
            Ask::Status => "device.status",
        }
    }

    fn request(self) -> &'static [u8] {
        match self {
            Ask::Version => VERSION_REQUEST,
            Ask::Status => STATUS_REQUEST,
        }
    }
}

/// Counters since the daemon started, for `Metric::status_*`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatusCounts {
    /// Requests handed to the writer.
    pub polls: u32,
    /// Replies to them read, refusals included.
    pub replies: u32,
    /// Requests given up unanswered.
    pub timeouts: u32,
    /// Lines on the channel that could not be read, replies cut short, and
    /// replies of ours that carried nothing usable.
    pub errors: u32,
}

/// The board and charge requests of one link at a time (module comment).
#[derive(Debug)]
pub struct StatusPoll {
    /// `None`: `status_poll_s = 0`, nothing is ever asked.
    every: Option<Duration>,
    /// When each request is next due on this link. `None` once it needs no
    /// asking (the board was answered) or must not be asked (the deck
    /// refused it, no link is up, or polling is off).
    version_due: Option<Instant>,
    status_due: Option<Instant>,
    /// The request awaiting its reply, and when it was handed over.
    in_flight: Option<(Ask, Instant)>,
    /// Another program is talking to the deck: nothing of ours before this.
    quiet_until: Option<Instant>,
    /// Channel-2 bytes since the last line feed.
    line: Vec<u8>,
    counts: StatusCounts,
}

impl StatusPoll {
    /// `every`: how often the charge is asked; `None` asks nothing at all.
    pub fn new(every: Option<Duration>) -> Self {
        StatusPoll {
            every,
            version_due: None,
            status_due: None,
            in_flight: None,
            quiet_until: None,
            line: Vec::new(),
            counts: StatusCounts::default(),
        }
    }

    pub fn counts(&self) -> StatusCounts {
        self.counts
    }

    /// A link opened: the board and then the charge are due as soon as its
    /// session is live.
    pub fn start(&mut self, now: Instant) {
        self.stop();
        self.version_due = self.every.map(|_| now);
        self.status_due = self.every.map(|_| now);
    }

    /// The link ended: nothing in flight survives it, a reply it cut short
    /// included, and nothing is due until the next one opens.
    pub fn stop(&mut self) {
        self.version_due = None;
        self.status_due = None;
        self.in_flight = None;
        self.quiet_until = None;
        self.line.clear();
    }

    /// The request to write now, if one is due: the board before the
    /// charge, one at a time. Asking only while the session is live is the
    /// caller's part. A request is due again a period after it was handed
    /// over, so one that goes unanswered is asked again on time.
    pub fn next_request(&mut self, now: Instant) -> Option<[u8; REPORT_LEN]> {
        let every = self.every?;
        if self.in_flight.is_some() || self.quiet_until.is_some_and(|until| now < until) {
            return None;
        }
        let due = |at: Option<Instant>| at.is_some_and(|at| at <= now);
        let ask = if due(self.version_due) {
            self.version_due = Some(now + every);
            Ask::Version
        } else if due(self.status_due) {
            self.status_due = Some(now + every);
            Ask::Status
        } else {
            return None;
        };
        self.in_flight = Some((ask, now));
        self.counts.polls += 1;
        Some(request_report(ask.request()))
    }

    /// The request in flight is given up after [`STATUS_TIMEOUT`].
    pub fn tick(&mut self, now: Instant) {
        let Some((ask, at)) = self.in_flight else {
            return;
        };
        if now.saturating_duration_since(at) < STATUS_TIMEOUT {
            return;
        }
        self.in_flight = None;
        self.counts.timeouts += 1;
        // A reply that began and never ended would swallow the next one.
        self.line.clear();
        tracing::debug!(method = ask.method(), "no answer on channel 2");
    }

    /// One channel-2 input report: what the replies it completes say.
    pub fn on_report(&mut self, report: &[u8], now: Instant) -> Vec<DeviceReport> {
        let mut said = Vec::new();
        let Some(payload) = payload(report) else {
            return said;
        };
        // A reply always begins a report with `{"id":`, so whatever is
        // still waiting for its line feed then was cut short (the deck
        // drops a partial request on `{"method"` the same way).
        if !self.line.is_empty() && payload.starts_with(b"{\"id\":") {
            self.counts.errors += 1;
            self.line.clear();
        }
        self.line.extend_from_slice(payload);
        while let Some(end) = self.line.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.line.drain(..=end).collect();
            self.read(&line[..end], now, &mut said);
        }
        if self.line.len() > LINE_MAX {
            self.counts.errors += 1;
            self.line.clear();
        }
        said
    }

    /// One whole line from the channel.
    fn read(&mut self, line: &[u8], now: Instant, said: &mut Vec<DeviceReport>) {
        if line.trim_ascii().is_empty() {
            return;
        }
        let Ok(reply) = serde_json::from_slice::<Value>(line) else {
            self.counts.errors += 1;
            tracing::debug!(bytes = line.len(), "an unreadable line on channel 2");
            return;
        };
        let Some(ask) = reply.get("id").and_then(Value::as_u64).and_then(Ask::of) else {
            // Someone else's reply (the updater's, a bench tool's): theirs to
            // read, and a sign they are mid-conversation with the deck.
            self.quiet_until = Some(now + FOREIGN_QUIET);
            return;
        };
        // A late reply, after its request was given up, still says what
        // the deck said; it only ends the wait when it is the one awaited.
        if self.in_flight.is_some_and(|(awaited, _)| awaited == ask) {
            self.in_flight = None;
        }
        self.counts.replies += 1;
        if let Some(error) = reply.get("error") {
            if error.get("code").and_then(Value::as_i64) == Some(METHOD_NOT_FOUND) {
                // A deck without the method will not learn it on this link.
                *self.due_mut(ask) = None;
                tracing::info!(
                    method = ask.method(),
                    "the deck does not know this method; not asked again on this link"
                );
            } else {
                tracing::debug!(method = ask.method(), %error, "the deck refused a status request");
            }
            return;
        }
        let Some(result) = reply.get("result").filter(|result| result.is_object()) else {
            self.counts.errors += 1;
            return;
        };
        match ask {
            Ask::Version => {
                self.version_due = None;
                let board = result.get("board").and_then(Value::as_str);
                said.push(DeviceReport::Board(board.map(str::to_owned)));
            }
            Ask::Status => {
                // The deck sends 0 when it has no reading; the store makes
                // that unknown, so it is passed on as said.
                let battery = result.get("battery").and_then(Value::as_u64);
                said.push(DeviceReport::Power {
                    battery: battery.map(|percent| percent.min(100) as u8),
                    charging: result.get("is_charging").and_then(Value::as_bool),
                });
            }
        }
    }

    fn due_mut(&mut self, ask: Ask) -> &mut Option<Instant> {
        match ask {
            Ask::Version => &mut self.version_due,
            Ask::Status => &mut self.status_due,
        }
    }
}

/// A request as the one output report that carries it: `[6, 2, len, …]`.
fn request_report(request: &[u8]) -> [u8; REPORT_LEN] {
    let mut report = [0; REPORT_LEN];
    report[0] = REPORT_ID;
    report[1] = CHANNEL;
    report[2] = request.len() as u8;
    report[3..3 + request.len()].copy_from_slice(request);
    report
}

/// The payload of a channel-2 input report, with or without its report id
/// (the rule of `hid::channel_of`).
fn payload(report: &[u8]) -> Option<&[u8]> {
    let body = match report {
        [REPORT_ID, body @ ..] => body,
        body => body,
    };
    match body {
        [CHANNEL, len, rest @ ..] => rest.get(..usize::from(*len).min(PAYLOAD_MAX)),
        _ => None,
    }
}

// The deck's own side of channel 2, for the tests: the firmware's request
// reassembler, handler and reply framing, compiled from its source. It sits
// here, at the top of the file, because a `#[path]` inside `mod tests`
// would resolve under `src/device/status/tests/`. The firmware's six tests
// of its own run with it.
#[cfg(test)]
#[rustfmt::skip]
#[allow(warnings, clippy::all)] // `wire.rs` names its own `audio-usb-rate-test` feature
#[path = "../../../../../src/link/wire.rs"] // host/crates/deckd/src/device/ → the repository
mod fw_wire;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::writer::{Writer, WriterCmd, Written};
    use crate::hid::tests::{cfg, feed, written, HELLO_BOOT, HELLO_MINE};
    use crate::hid::{FromHid, HidConfig, Link};
    use crossbeam_channel::{Receiver, Sender};
    use deck_model::{LinkEvent, SessionState, Transport};
    use hidapi::BusType;
    use serde_json::json;

    /// What a 0.13.0 Rev B deck calls itself on channel 2.
    const FW: &str = "0.13.0-openwalkietalkie";
    const BOARD: &str = "Rev B";

    fn text(bytes: &[u8]) -> &str {
        std::str::from_utf8(bytes).unwrap()
    }

    /// The deck's charge as the firmware's handler takes it.
    fn charge(battery: u8, is_charging: bool) -> fw_wire::Status {
        fw_wire::Status {
            battery,
            is_charging,
            recovery_protocol: Some(2),
            maintenance: false,
        }
    }

    /// The writer thread's other end, played by hand: each report the link
    /// hands over is taken, and its write returns at once.
    struct Wire {
        writer: Writer,
        handed: Receiver<WriterCmd>,
        done: Sender<Written>,
    }

    impl Wire {
        fn new() -> Wire {
            let ((cmds, handed), (done, finished)) = Writer::channels();
            Wire {
                writer: Writer::new(cmds, finished, None),
                handed,
                done,
            }
        }

        /// The report the link hands the writer at `t`, if it has one.
        fn pump(&mut self, link: &mut Link, t: Instant) -> Option<[u8; REPORT_LEN]> {
            assert_eq!(link.pump_writer(&mut self.writer, t), None);
            let Ok(WriterCmd::Write(report)) = self.handed.try_recv() else {
                return None;
            };
            let returned = Written {
                took: Duration::from_micros(700),
                ok: true,
            };
            self.done.send(returned).unwrap();
            assert_eq!(link.poll_writer(&mut self.writer, t), None);
            Some(report)
        }
    }

    /// Everything the hid loop hands the writer from `t` on, in order, the
    /// way `written` collects the outbox's lines (8 ms a report, until
    /// nothing more is due): deck lines start `{"m":`, and a channel-2
    /// request `{"method":`, line feed included.
    fn pumped(link: &mut Link, wire: &mut Wire, t: &mut Instant) -> Vec<String> {
        let (mut sent, mut partial) = (Vec::new(), Vec::new());
        let mut idle = 0;
        while idle < 2 {
            link.tick(*t);
            match wire.pump(link, *t) {
                Some(report) => {
                    idle = 0;
                    let bytes = &report[3..3 + usize::from(report[2])];
                    if report[1] == CHANNEL {
                        sent.push(text(bytes).to_string());
                    } else {
                        partial.extend_from_slice(bytes);
                        if partial.last() == Some(&b'\n') {
                            partial.pop();
                            sent.push(String::from_utf8(std::mem::take(&mut partial)).unwrap());
                        }
                    }
                }
                None => idle += 1,
            }
            *t += Duration::from_millis(8);
        }
        sent
    }

    fn asks(sent: &[String]) -> Vec<&str> {
        sent.iter()
            .map(String::as_str)
            .filter(|line| line.starts_with("{\"method\""))
            .collect()
    }

    /// A healthy link from `t` to `until`, a second at a time, each hello it
    /// writes answered: the channel-2 requests that went out, and when.
    fn run(
        link: &mut Link,
        wire: &mut Wire,
        t: &mut Instant,
        until: Instant,
    ) -> Vec<(Instant, String)> {
        let mut asked = Vec::new();
        while *t < until {
            let at = *t;
            let sent = pumped(link, wire, t);
            if sent.iter().any(|line| line.starts_with("{\"m\":\"hello\"")) {
                feed(link, HELLO_MINE, *t);
            }
            asked.extend(asks(&sent).into_iter().map(|ask| (at, ask.to_string())));
            *t = (*t).max(at + Duration::from_secs(1));
        }
        asked
    }

    /// A link opened on `bus` whose deck answered our sid: live, its hello
    /// written, the core's messages so far drained.
    fn live(cfg: &HidConfig, bus: BusType) -> (Link, Wire, Instant) {
        let mut link = Link::new(cfg);
        let mut t = Instant::now();
        link.link_up(t);
        link.opened(bus);
        feed(&mut link, HELLO_MINE, t);
        written(&mut link, &mut t);
        assert_eq!(link.session().state(), SessionState::Live);
        link.drain();
        (link, Wire::new(), t)
    }

    /// A reply as the deck's input reports: the JSON and one line feed, 61
    /// bytes a report, as the firmware's `frame` cuts it.
    fn reply_reports(json: &str) -> Vec<[u8; REPORT_LEN]> {
        let mut line = json.as_bytes().to_vec();
        line.push(b'\n');
        line.chunks(PAYLOAD_MAX)
            .map(|piece| {
                let mut report = [0; REPORT_LEN];
                report[..3].copy_from_slice(&[REPORT_ID, CHANNEL, piece.len() as u8]);
                report[3..3 + piece.len()].copy_from_slice(piece);
                report
            })
            .collect()
    }

    fn reply(link: &mut Link, json: &str, t: Instant) {
        for report in reply_reports(json) {
            link.on_report(&report, t);
        }
    }

    /// The deck's own answer to `request`, as text: the firmware's
    /// reassembler and handler, its reply in the reports the firmware cuts
    /// it into.
    fn firmware_reply(
        request: &[u8; REPORT_LEN],
        status: fw_wire::Status,
    ) -> (String, Vec<[u8; REPORT_LEN]>) {
        let mut deck = fw_wire::Reassembler::new();
        let fw_wire::Push::Complete(n) = deck.push(&request[1..]) else {
            panic!("the deck did not take {:?} whole", text(&request[3..]));
        };
        let mut tx = [0; fw_wire::TX_CAP];
        let (_, len) = fw_wire::handle_request(deck.data(n), FW, BOARD, status, &mut tx);
        assert!(len > 0, "the reply fits the deck's buffer");
        let mut reports = Vec::new();
        let (mut off, mut report) = (0, [0; fw_wire::REPORT_LEN]);
        loop {
            let carried = fw_wire::frame(&tx[..len], off, &mut report);
            if carried == 0 {
                break;
            }
            off += carried;
            reports.push(report);
        }
        (text(&tx[..len]).to_string(), reports)
    }

    /// The deck answers `request` and the link reads the answer.
    fn firmware_answers(
        link: &mut Link,
        request: &[u8; REPORT_LEN],
        status: fw_wire::Status,
        t: Instant,
    ) -> String {
        let (reply, reports) = firmware_reply(request, status);
        for report in &reports {
            link.on_report(report, t);
        }
        reply
    }

    /// What the link told the core about the deck.
    fn said(link: &mut Link) -> Vec<DeviceReport> {
        link.drain()
            .into_iter()
            .filter_map(|message| match message {
                FromHid::Report(report) => Some(report),
                _ => None,
            })
            .collect()
    }

    /// The contract with the firmware, on its own source: each request is
    /// one report the deck's reassembler takes whole, the deck's handler
    /// answers it, and its reply — cut where the firmware cuts it — reads
    /// back as the board and the charge. A deck without the method answers
    /// `-32601`, and is not asked it again.
    #[test]
    fn each_request_is_one_report_the_firmware_takes_whole_and_its_answer_reads_back() {
        assert!(text(STATUS_REQUEST).contains(&STATUS_ID.to_string()));
        assert!(text(VERSION_REQUEST).contains(&VERSION_ID.to_string()));
        let (mut link, mut wire, t) = live(&cfg(), BusType::Usb);
        let version = wire.pump(&mut link, t).expect("the board is asked first");
        let len = VERSION_REQUEST.len();
        assert_eq!(version[..3], [REPORT_ID, CHANNEL, len as u8]);
        assert_eq!(text(&version[3..3 + len]), text(VERSION_REQUEST));
        assert!(version[3 + len..].iter().all(|byte| *byte == 0));
        let answer = firmware_answers(&mut link, &version, charge(85, true), t);
        assert_eq!(
            answer,
            r#"{"id":47302,"result":{"version":"0.13.0-openwalkietalkie","board":"Rev B","recovery_protocol":2}}"#
        );
        assert_eq!(said(&mut link), [DeviceReport::Board(Some(BOARD.into()))]);
        let status = wire.pump(&mut link, t).expect("then the charge, at once");
        assert!(text(&status[3..]).starts_with("{\"method\":\"device.status\""));
        let answer = firmware_answers(&mut link, &status, charge(85, true), t);
        assert_eq!(
            answer,
            r#"{"id":47301,"result":{"version":"0.13.0-openwalkietalkie","battery":85,"is_charging":true}}"#
        );
        assert_eq!(
            said(&mut link),
            [DeviceReport::Power {
                battery: Some(85),
                charging: Some(true)
            }]
        );
        // A deck whose firmware has no `device.status`: the same request,
        // renamed for the deck's handler, which answers it the way it
        // answers anything it does not know.
        let mut renamed = wire.pump(&mut link, t + STATUS_EVERY).expect("due again");
        let at = text(&renamed).find("device.status").unwrap();
        renamed[at + "device.statu".len()] = b'z';
        let answer = firmware_answers(&mut link, &renamed, charge(85, true), t + STATUS_EVERY);
        assert_eq!(
            answer,
            r#"{"id":47301,"error":{"code":-32601,"message":"Method not found"}}"#
        );
        assert!(said(&mut link).is_empty());
        assert_eq!(
            wire.pump(&mut link, t + STATUS_EVERY * 4),
            None,
            "not asked again"
        );
        let m = link.metric(t);
        assert_eq!((m.status_polls, m.status_replies), (3, 3));
    }

    /// Nothing is asked before the session is live: not while the hello is
    /// unanswered, and not during the barrier a fresh deck needs. The board
    /// goes out right after the barrier's `synced`, the charge once the
    /// board is answered.
    #[test]
    fn nothing_is_asked_before_the_session_is_live() {
        let mut link = Link::new(&cfg());
        let mut wire = Wire::new();
        let mut t = Instant::now();
        link.link_up(t);
        link.opened(BusType::Bluetooth);
        let sent = pumped(&mut link, &mut wire, &mut t);
        assert_eq!(sent, ["{\"m\":\"hello\",\"v\":1,\"sid\":314159}"]);
        t += Duration::from_millis(1500);
        assert!(
            pumped(&mut link, &mut wire, &mut t).is_empty(),
            "awaiting the hello"
        );
        feed(&mut link, HELLO_BOOT, t);
        assert_eq!(link.session().state(), SessionState::Syncing);
        let sent = pumped(&mut link, &mut wire, &mut t);
        let synced = sent.iter().position(|line| line == "{\"m\":\"synced\"}");
        let synced = synced.unwrap_or_else(|| panic!("{sent:?}"));
        assert_eq!(asks(&sent), [text(VERSION_REQUEST)], "{sent:?}");
        assert_eq!(sent.len(), synced + 2, "the board right after the barrier");
        assert_eq!(link.session().state(), SessionState::Live);
        let board = r#"{"id":47302,"result":{"version":"0.1.0-openwalkietalkie","board":"Rev A"}}"#;
        reply(&mut link, board, t);
        assert_eq!(
            asks(&pumped(&mut link, &mut wire, &mut t)),
            [text(STATUS_REQUEST)]
        );
    }

    /// A deck line that is due goes before a request that is due: the
    /// deck's own traffic never waits for channel 2. The request rides
    /// outside the lines' pace, the way a voice frame does, and leaves it
    /// as it was.
    #[test]
    fn the_decks_lines_go_before_a_request_that_is_due_and_keep_their_pace() {
        use deck_model::{DeviceOp, HostLine, Key, SetKey};
        let brightness = |value| {
            let line = HostLine::Set {
                key: SetKey::Brightness,
                value,
            };
            DeviceOp::Put(Key::Set(SetKey::Brightness), line)
        };
        let (mut link, mut wire, t) = live(&cfg(), BusType::Usb);
        link.apply(brightness(2), t);
        let line = wire.pump(&mut link, t).expect("the line first");
        assert_eq!(line[1], deck_proto::CHANNEL);
        let request = wire
            .pump(&mut link, t)
            .expect("then the board, at the same instant");
        assert_eq!(
            text(&request[3..3 + VERSION_REQUEST.len()]),
            text(VERSION_REQUEST)
        );
        link.apply(brightness(3), t);
        assert_eq!(
            wire.pump(&mut link, t),
            None,
            "the next line keeps the set's pace"
        );
        let next = wire
            .pump(&mut link, t + Duration::from_millis(8))
            .expect("and goes on time");
        assert_eq!(
            text(&next[3..3 + usize::from(next[2])]),
            "{\"m\":\"set\",\"k\":\"brightness\",\"v\":3}\n"
        );
        let m = link.metric(t);
        assert_eq!(
            (m.lines_sent, m.status_polls),
            (3, 1),
            "the hello and two sets; the board"
        );
    }

    /// The board once a link, the charge at once and then every 30 s. Each
    /// reply is two reports, read by its line feed. A battery of 0 is
    /// passed on as the deck said it: the store makes it unknown.
    #[test]
    fn the_board_once_a_link_and_the_charge_at_once_then_every_thirty_seconds() {
        let (mut link, mut wire, mut t) = live(&cfg(), BusType::Usb);
        assert_eq!(
            asks(&pumped(&mut link, &mut wire, &mut t)),
            [text(VERSION_REQUEST)]
        );
        let board =
            r#"{"id":47302,"result":{"version":"0.13.0-openwalkietalkie","board":"Rev A"}}"#;
        let reports = reply_reports(board);
        assert_eq!(reports.len(), 2);
        for report in &reports {
            link.on_report(report, t);
        }
        assert_eq!(said(&mut link), [DeviceReport::Board(Some("Rev A".into()))]);
        let asked = t;
        assert_eq!(
            asks(&pumped(&mut link, &mut wire, &mut t)),
            [text(STATUS_REQUEST)]
        );
        let charge = r#"{"id":47301,"result":{"version":"0.13.0-openwalkietalkie","battery":0,"is_charging":false}}"#;
        let reports = reply_reports(charge);
        assert_eq!(reports.len(), 2);
        link.on_report(&reports[0], t);
        assert!(said(&mut link).is_empty(), "nothing before the line feed");
        link.on_report(&reports[1], t);
        assert_eq!(
            said(&mut link),
            [DeviceReport::Power {
                battery: Some(0),
                charging: Some(false)
            }]
        );
        let early = asked + STATUS_EVERY - Duration::from_secs(1);
        let seen = run(&mut link, &mut wire, &mut t, early);
        assert!(seen.is_empty(), "{seen:?}");
        let again = run(
            &mut link,
            &mut wire,
            &mut t,
            asked + STATUS_EVERY + Duration::from_secs(1),
        );
        let again: Vec<&str> = again.iter().map(|(_, ask)| ask.as_str()).collect();
        assert_eq!(
            again,
            [text(STATUS_REQUEST)],
            "the charge only: the board is known"
        );
        assert_eq!(link.session().state(), SessionState::Live);
        let m = link.metric(t);
        assert_eq!(
            (m.status_polls, m.status_replies, m.status_timeouts),
            (3, 2, 0)
        );
        assert!(m
            .summary()
            .contains("status polls=3 replies=2 timeouts=0 errors=0"));
    }

    /// A 61-byte reply sends its line feed alone (the `-32601` of a request
    /// with a one-digit id, as a bench tool makes one): nothing is read
    /// until that second report, and then it is someone else's reply, which
    /// holds ours back a while. Our own `-32601` stops that method, and only
    /// that one, for the rest of the link.
    #[test]
    fn a_line_feed_alone_ends_a_reply_and_a_refused_method_is_not_asked_again_on_this_link() {
        let (mut link, mut wire, mut t) = live(&cfg(), BusType::Usb);
        let theirs = r#"{"id":7,"error":{"code":-32601,"message":"Method not found"}}"#;
        assert_eq!(theirs.len(), PAYLOAD_MAX);
        let reports = reply_reports(theirs);
        assert_eq!(
            reports[1][..4],
            [REPORT_ID, CHANNEL, 1, b'\n'],
            "the line feed alone"
        );
        link.on_report(&reports[0], t);
        assert_eq!(
            asks(&pumped(&mut link, &mut wire, &mut t)),
            [text(VERSION_REQUEST)]
        );
        link.on_report(&reports[1], t);
        assert!(said(&mut link).is_empty(), "not ours");
        // The board refused: not asked again, and the charge still is.
        let refused = r#"{"id":47302,"error":{"code":-32601,"message":"Method not found"}}"#;
        reply(&mut link, refused, t);
        assert!(said(&mut link).is_empty());
        let sent = pumped(&mut link, &mut wire, &mut t);
        assert_eq!(
            asks(&sent),
            [] as [&str; 0],
            "quiet after someone else's reply"
        );
        t += FOREIGN_QUIET;
        assert_eq!(
            asks(&pumped(&mut link, &mut wire, &mut t)),
            [text(STATUS_REQUEST)]
        );
        reply(&mut link, &refused.replace("47302", "47301"), t);
        let minute = t + Duration::from_secs(60);
        let seen = run(&mut link, &mut wire, &mut t, minute);
        assert!(seen.is_empty(), "{seen:?}");
        assert_eq!(link.metric(t).status_replies, 2);
        // A new link asks both again.
        link.link_down(t, "removed");
        link.link_up(t);
        feed(&mut link, HELLO_MINE, t);
        let sent = pumped(&mut link, &mut wire, &mut t);
        assert_eq!(asks(&sent), [text(VERSION_REQUEST)], "{sent:?}");
    }

    /// A reply with someone else's id — the updater's `firmware.check` asks
    /// as 47201 from its own handle, and its replies reach ours — is
    /// ignored while ours is still awaited, and holds our next request back
    /// until the other program has been quiet for a while.
    #[test]
    fn another_programs_reply_is_ignored_and_holds_our_next_request_back() {
        let (mut link, mut wire, mut t) = live(&cfg(), BusType::Usb);
        assert_eq!(
            asks(&pumped(&mut link, &mut wire, &mut t)),
            [text(VERSION_REQUEST)]
        );
        reply(&mut link, r#"{"id":47302,"result":{"board":"Rev B"}}"#, t);
        assert_eq!(said(&mut link), [DeviceReport::Board(Some(BOARD.into()))]);
        let asked = t;
        assert_eq!(
            asks(&pumped(&mut link, &mut wire, &mut t)),
            [text(STATUS_REQUEST)]
        );
        let theirs = r#"{"id":47201,"result":{"version":"0.13.0-openwalkietalkie","board":"Rev B","recovery_protocol":2}}"#;
        reply(&mut link, theirs, t);
        assert!(said(&mut link).is_empty());
        let ours = r#"{"id":47301,"result":{"version":"0.13.0-openwalkietalkie","battery":55,"is_charging":true}}"#;
        reply(&mut link, ours, t);
        assert_eq!(
            said(&mut link),
            [DeviceReport::Power {
                battery: Some(55),
                charging: Some(true)
            }],
            "ours is still read"
        );
        // Due 30 s after the last; the other program speaks again 25 s in,
        // so ours waits until it has been quiet for ten.
        let seen = run(
            &mut link,
            &mut wire,
            &mut t,
            asked + Duration::from_secs(25),
        );
        assert!(seen.is_empty(), "{seen:?}");
        reply(&mut link, theirs, t);
        let spoke = t;
        let seen = run(
            &mut link,
            &mut wire,
            &mut t,
            spoke + FOREIGN_QUIET + Duration::from_secs(2),
        );
        assert_eq!(seen.len(), 1, "{seen:?}");
        let (when, ask) = &seen[0];
        assert_eq!(ask, text(STATUS_REQUEST));
        assert!(*when > asked + STATUS_EVERY, "held back past its time");
        assert!(
            *when >= spoke + FOREIGN_QUIET
                && *when < spoke + FOREIGN_QUIET + Duration::from_secs(1)
        );
        assert_eq!(
            link.metric(t).status_errors,
            0,
            "someone else's reply is no error"
        );
    }

    /// No reply is an outcome: the request is given up after two seconds,
    /// the next goes, and each is asked again a period after it went.
    #[test]
    fn an_unanswered_request_is_given_up_and_the_next_poll_still_goes() {
        let (mut link, mut wire, mut t) = live(&cfg(), BusType::Bluetooth);
        let started = t;
        assert_eq!(
            asks(&pumped(&mut link, &mut wire, &mut t)),
            [text(VERSION_REQUEST)]
        );
        t = started + STATUS_TIMEOUT - Duration::from_millis(1);
        link.tick(t);
        assert_eq!(link.metric(t).status_timeouts, 0);
        assert_eq!(wire.pump(&mut link, t), None, "one at a time");
        t = started + STATUS_TIMEOUT + Duration::from_millis(20);
        let sent = pumped(&mut link, &mut wire, &mut t);
        assert_eq!(
            asks(&sent),
            [text(STATUS_REQUEST)],
            "the charge, the board given up"
        );
        assert_eq!(link.metric(t).status_timeouts, 1);
        let until = started + STATUS_EVERY + STATUS_TIMEOUT * 3;
        let seen = run(&mut link, &mut wire, &mut t, until);
        let seen: Vec<&str> = seen.iter().map(|(_, ask)| ask.as_str()).collect();
        assert_eq!(
            seen,
            [text(VERSION_REQUEST), text(STATUS_REQUEST)],
            "both again, on time"
        );
        let m = link.metric(t);
        assert_eq!((m.status_polls, m.status_replies), (4, 0));
        assert!(m.status_timeouts >= 3, "{m:?}");
        assert_eq!(
            link.session().state(),
            SessionState::Live,
            "the link itself is fine"
        );
    }

    /// A link that drops mid-reply takes the reply with it: the rest of it,
    /// arriving on the next link, is no reply, and the next link asks again
    /// from the start.
    #[test]
    fn a_reply_cut_by_a_drop_goes_with_the_link() {
        let (mut link, mut wire, mut t) = live(&cfg(), BusType::Bluetooth);
        let version = wire.pump(&mut link, t).expect("the board");
        let (answer, reports) = firmware_reply(&version, charge(85, false));
        assert_eq!(reports.len(), 2);
        link.on_report(&reports[0], t);
        link.link_down(t, "read");
        link.drain();
        t += Duration::from_millis(800);
        link.link_up(t);
        link.on_report(&reports[1], t);
        assert!(said(&mut link).is_empty(), "half a reply says nothing");
        assert_eq!(link.metric(t).status_errors, 1);
        feed(&mut link, HELLO_MINE, t);
        let sent = pumped(&mut link, &mut wire, &mut t);
        assert_eq!(
            asks(&sent),
            [text(VERSION_REQUEST)],
            "the new link asks again"
        );
        reply(&mut link, &answer, t);
        assert_eq!(said(&mut link), [DeviceReport::Board(Some(BOARD.into()))]);
    }

    /// The bus the deck was opened on is its transport, said after the
    /// link's `Linked` (the store ignores a report about a deck that is not
    /// linked); a bus hidapi cannot name is no transport.
    #[test]
    fn the_bus_a_deck_is_opened_on_is_its_transport() {
        for (bus, transport) in [
            (BusType::Usb, Some(Transport::Usb)),
            (BusType::Bluetooth, Some(Transport::Bluetooth)),
            (BusType::Unknown, None),
        ] {
            let mut link = Link::new(&cfg());
            link.link_up(Instant::now());
            link.opened(bus);
            assert_eq!(
                link.drain(),
                [
                    FromHid::Link(LinkEvent::Linked),
                    FromHid::Report(DeviceReport::Transport(transport)),
                ]
            );
        }
    }

    /// However a link ends, the core hears why just before the `Down`: the
    /// hid thread's read, write, stall and removal reasons, the install that
    /// closes it on purpose, and six unanswered hellos.
    #[test]
    fn every_way_a_link_ends_says_why_just_before_the_down() {
        fn why_it_ended(link: &mut Link) -> String {
            let messages = link.drain();
            let down = messages
                .iter()
                .position(|m| *m == FromHid::Link(LinkEvent::Down));
            let down = down.unwrap_or_else(|| panic!("no Down in {messages:?}"));
            let whys: Vec<&FromHid> = messages
                .iter()
                .filter(|m| matches!(m, FromHid::Report(DeviceReport::Dropped { .. })))
                .collect();
            assert_eq!(whys.len(), 1, "{messages:?}");
            match &messages[down - 1] {
                FromHid::Report(DeviceReport::Dropped { why }) => why.clone(),
                other => panic!("{other:?} just before the Down in {messages:?}"),
            }
        }
        for why in ["read", "write", "write_stall", "removed", "firmware_update"] {
            let (mut link, mut wire, t) = live(&cfg(), BusType::Usb);
            link.link_down(t, why);
            assert_eq!(why_it_ended(&mut link), why);
            link.link_down(t, why);
            assert!(link.drain().is_empty(), "a link ends once");
            // A closed device (an install holds it closed) is asked nothing.
            assert_eq!(wire.pump(&mut link, t + STATUS_EVERY), None);
        }
        let (mut link, mut wire, mut t) = live(&cfg(), BusType::Bluetooth);
        for _ in 0..6 {
            t += Duration::from_secs(5);
            pumped(&mut link, &mut wire, &mut t);
            t += Duration::from_secs(2);
            link.tick(t);
        }
        assert!(link.take_drop_handle());
        assert_eq!(why_it_ended(&mut link), "misses");
    }

    /// `[device] status_poll_s = 0`: a live deck is asked nothing on
    /// channel 2, however long it stays.
    #[test]
    fn with_status_poll_s_at_zero_nothing_is_asked() {
        let off = HidConfig {
            status_every: None,
            ..cfg()
        };
        let (mut link, mut wire, mut t) = live(&off, BusType::Usb);
        let later = t + STATUS_EVERY * 5;
        let seen = run(&mut link, &mut wire, &mut t, later);
        assert!(seen.is_empty(), "{seen:?}");
        assert_eq!(link.session().state(), SessionState::Live);
        assert_eq!(link.metric(t).status_polls, 0);
        assert!(said(&mut link).is_empty());
    }

    /// Anything on the channel that cannot be read is counted and passed
    /// over; a line that never ends is dropped once it outgrows any reply,
    /// and the next reply is still read whole.
    #[test]
    fn an_unreadable_line_is_counted_and_passed_over() {
        let (mut link, mut wire, mut t) = live(&cfg(), BusType::Usb);
        assert_eq!(
            asks(&pumped(&mut link, &mut wire, &mut t)),
            [text(VERSION_REQUEST)]
        );
        reply(&mut link, "not json at all", t);
        reply(&mut link, r#"{"id":47302,"result":"Rev B"}"#, t);
        assert!(said(&mut link).is_empty());
        assert_eq!(link.metric(t).status_errors, 2);
        let sent = pumped(&mut link, &mut wire, &mut t);
        assert_eq!(asks(&sent), [text(STATUS_REQUEST)], "the charge goes on");
        let endless = reply_reports(&"x".repeat(LINE_MAX + 100));
        for report in &endless[..endless.len() - 1] {
            link.on_report(report, t);
        }
        assert_eq!(link.metric(t).status_errors, 3);
        reply(
            &mut link,
            r#"{"id":47301,"result":{"battery":40,"is_charging":false}}"#,
            t,
        );
        assert_eq!(
            said(&mut link),
            [DeviceReport::Power {
                battery: Some(40),
                charging: Some(false)
            }]
        );
        assert_eq!(
            link.metric(t).status_errors,
            4,
            "the leftover was dropped, not glued on"
        );
        let mut diagnostics = [0; REPORT_LEN];
        diagnostics[..4].copy_from_slice(&[REPORT_ID, 4, 1, b'\n']);
        assert_eq!(payload(&diagnostics), None, "channel 4 is not read here");
        assert_eq!(
            payload(&[CHANNEL, 2, b'{', b'}', 0]),
            Some(&b"{}"[..]),
            "no report id"
        );
    }

    /// B3.3: a deck answering on channel 2 — the firmware's own handler —
    /// reaches the bus as `device.status` events that carry every key of
    /// the app's fixture of this event, each with the JSON type the app's
    /// mirror reads.
    #[test]
    fn a_deck_answering_on_channel_two_reaches_the_bus_in_the_shape_the_app_reads() {
        let dir = crate::core::testkit::temp("device-status");
        let (mut core, _ops, _lines, _debug) = crate::core::testkit::detached(&dir);
        let mut link = Link::new(&cfg());
        let mut wire = Wire::new();
        let mut t = Instant::now();
        // A link lost before: the drop the status keeps.
        link.link_up(t);
        link.opened(BusType::Usb);
        link.link_down(t, "read");
        // This one: over Bluetooth, live, asked and answered.
        t += Duration::from_secs(1);
        link.link_up(t);
        link.opened(BusType::Bluetooth);
        feed(&mut link, HELLO_MINE, t);
        written(&mut link, &mut t);
        for _ in 0..2 {
            let request = wire.pump(&mut link, t).expect("a request is due");
            firmware_answers(&mut link, &request, charge(85, false), t);
        }
        let deliver = |link: &mut Link, core: &mut crate::core::Core, t: Instant| {
            for message in link.drain() {
                core.on_from_hid(message, t);
            }
            core.publish();
            let (events, _) = core
                .bus
                .replay(0, 500, Some(&["device.status".to_string()]));
            events
                .iter()
                .map(|event| event.data.clone())
                .collect::<Vec<Value>>()
        };
        let events = deliver(&mut link, &mut core, t);
        let data = events.last().expect("device.status");
        assert_eq!(data["transport"], "bluetooth");
        assert_eq!(
            (&data["battery"], &data["charging"]),
            (&json!(85), &json!(false))
        );
        assert_eq!(data["board"], BOARD);
        assert_eq!(data["last"]["board"], BOARD);
        assert_eq!(data["last"]["transport"], "bluetooth");
        assert_eq!(data["drops"][0]["why"], "read");
        let fixture: Value = serde_json::from_str(APP_FIXTURE).unwrap();
        same_shape(&fixture, data, "device_status.json").unwrap();
        let device = &core.store.snapshot().device;
        assert_eq!((device.battery, device.charging), (Some(85), Some(false)));
        // The deck loses its reading: 0 on the wire, unknown here, in one
        // more event.
        let later = t + STATUS_EVERY;
        let request = wire.pump(&mut link, later).expect("the next poll");
        firmware_answers(&mut link, &request, charge(0, false), later);
        let after = deliver(&mut link, &mut core, later);
        assert_eq!(after.len(), events.len() + 1);
        assert_eq!(after.last().unwrap()["battery"], Value::Null);
        assert_eq!(core.store.snapshot().device.battery, None);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The app's fixture of `device.status`'s data, as its mirror's own
    /// tests read it.
    const APP_FIXTURE: &str =
        include_str!("../../../deck-gui-core/tests/fixtures/daemon/device_status.json");

    /// Every key of `want` is in `got` with the same JSON type, all the way
    /// down (an array by its first element). A null on either side is a
    /// value nobody knows yet, and says nothing about its type.
    fn same_shape(want: &Value, got: &Value, at: &str) -> Result<(), String> {
        match (want, got) {
            (Value::Null, _) | (_, Value::Null) => Ok(()),
            (Value::Object(want), Value::Object(got)) => {
                want.iter().try_for_each(|(key, value)| {
                    let found = got
                        .get(key)
                        .ok_or_else(|| format!("{at}.{key} is missing"))?;
                    same_shape(value, found, &format!("{at}.{key}"))
                })
            }
            (Value::Array(want), Value::Array(got)) => match (want.first(), got.first()) {
                (Some(want), Some(got)) => same_shape(want, got, &format!("{at}[0]")),
                _ => Ok(()),
            },
            (Value::Bool(_), Value::Bool(_))
            | (Value::Number(_), Value::Number(_))
            | (Value::String(_), Value::String(_)) => Ok(()),
            _ => Err(format!(
                "{at}: the service sends {got} where the app reads {want}"
            )),
        }
    }

    #[test]
    fn the_shape_check_names_a_missing_key_and_a_changed_type() {
        let want = json!({"a": 1, "b": {"c": "x"}, "d": [{"e": true}], "f": null});
        let fine = json!({"a": 2, "b": {"c": "y"}, "d": [], "f": 3, "g": "more"});
        assert_eq!(same_shape(&want, &fine, "x"), Ok(()));
        let missing = json!({"a": 2, "b": {}, "d": []});
        assert_eq!(
            same_shape(&want, &missing, "x"),
            Err("x.b.c is missing".into())
        );
        let retyped = json!({"a": "2", "b": {"c": "y"}, "d": [], "f": 1});
        assert_eq!(
            same_shape(&want, &retyped, "x"),
            Err("x.a: the service sends \"2\" where the app reads 1".into())
        );
        let deep = json!({"a": 1, "b": {"c": "y"}, "d": [{"e": 0}], "f": 1});
        assert!(same_shape(&want, &deep, "x")
            .unwrap_err()
            .starts_with("x.d[0].e:"));
    }
}
