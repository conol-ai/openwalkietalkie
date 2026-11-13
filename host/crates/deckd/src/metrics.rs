//! Link and pacer statistics (`metrics.get`): the hid thread reports a
//! `Metric` every 30 s (`FromHid::Metric`), the core logs it and mirrors
//! it with the device state into `status.json`, which `deckd status` reads.
use deck_model::{ErrCounts, Link, SessionState, Transport, Ts};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

/// How often the hid thread reports and the core writes `status.json`.
pub const REPORT_EVERY: Duration = Duration::from_secs(30);

/// Counters since the daemon started, plus the queue depths of the moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Metric {
    pub session: SessionState,
    pub pace_ms: u16,
    pub backoff: bool,
    pub outbox_depth: u32,
    pub p0: u32,
    pub p1: u32,
    pub p2: u32,
    pub p3: u32,
    pub barrier: u32,
    pub reports_sent: u64,
    pub reports_received: u64,
    pub lines_sent: u64,
    pub lines_received: u64,
    pub link_drops: u32,
    pub reopens: u32,
    pub barriers: u32,
    pub hello_sent: u32,
    pub hello_replies: u32,
    /// Misses of the current hello run (reset by every reply).
    pub hello_misses: u32,
    pub hello_misses_total: u32,
    pub err_size: u32,
    pub err_parse: u32,
    pub err_range: u32,
    pub err_unknown: u32,
    /// Device lines the assembler could not parse.
    pub decode_errors: u32,
    /// Host lines the encoder refused (bugs, never sent).
    pub encode_errors: u64,
    /// Input reports on channel 2 (system RPC), 4 (diagnostics), other.
    pub ch2_reports: u64,
    pub ch4_reports: u64,
    pub other_reports: u64,
    /// Input reports on channel 7 (the voice link over the cable).
    #[serde(default)]
    pub voice_reports: u64,
    pub flood_sent: u64,
    pub flood_remaining: u64,
    pub pace_backoffs: u32,
    /// `hid_write` latency: it blocks for the BLE round trip, which is what
    /// really paces the link (the 8 ms floor is never the limiter).
    pub write_avg_us: u32,
    pub write_max_us: u32,
    /// Writes that took longer than 100 ms.
    pub writes_slow: u32,
    /// Reports handed to `deckd-hid-write` at once. The hid loop asks
    /// `Link::next_report` only while the writer is idle, so this is 1 on
    /// any healthy link and never more; a 2 would be a bug in the hand-off.
    pub writes_in_flight_max: u32,
    /// Writes that never returned within `writer::WRITE_STALL` (a hung BLE
    /// write): the link was dropped with `write_stall` and the thread
    /// abandoned.
    pub write_stalls: u32,
    /// The deck's `answer` input report → the `card clear` report for that
    /// (slot, card) being handed to the writer. The host's whole share of
    /// the §4.8 budget; the target is < 100 ms (contract §5 row 8, §7 row 3).
    pub answer_to_clear_host_us: u32,
    pub answer_to_clear_host_max_us: u32,
    /// The same, to that report's `hid_write` returning: it adds the BLE
    /// round trip, which is what the deck actually waits for.
    pub answer_to_clear_wire_us: u32,
    pub answer_to_clear_wire_max_us: u32,
    /// Channel-2 requests for the deck's board and charge
    /// (`device::status`): handed to the writer, answered (refusals
    /// included), given up unanswered, and channel-2 lines that could not
    /// be read.
    #[serde(default)]
    pub status_polls: u32,
    #[serde(default)]
    pub status_replies: u32,
    #[serde(default)]
    pub status_timeouts: u32,
    #[serde(default)]
    pub status_errors: u32,
}

impl Metric {
    pub fn err_total(&self) -> u32 {
        self.err_size + self.err_parse + self.err_range + self.err_unknown
    }

    /// One log line's worth.
    pub fn summary(&self) -> String {
        format!(
            "session={:?} pace={}ms{} depth={} (p0={} p1={} p2={} p3={} barrier={}) reports tx={} rx={} lines tx={} rx={} hello tx={} rx={} misses={} link_drops={} barriers={} err={} (size={} parse={} range={} unknown={}) decode_err={} encode_err={} ch2={} ch4={} ch7={} status polls={} replies={} timeouts={} errors={} flood={}/{} backoffs={} write avg={}us max={}us slow={} in_flight_max={} stalls={} answer→clear host={}us (max {}) wire={}us (max {})",
            self.session,
            self.pace_ms,
            if self.backoff { " (backoff)" } else { "" },
            self.outbox_depth,
            self.p0,
            self.p1,
            self.p2,
            self.p3,
            self.barrier,
            self.reports_sent,
            self.reports_received,
            self.lines_sent,
            self.lines_received,
            self.hello_sent,
            self.hello_replies,
            self.hello_misses_total,
            self.link_drops,
            self.barriers,
            self.err_total(),
            self.err_size,
            self.err_parse,
            self.err_range,
            self.err_unknown,
            self.decode_errors,
            self.encode_errors,
            self.ch2_reports,
            self.ch4_reports,
            self.voice_reports,
            self.status_polls,
            self.status_replies,
            self.status_timeouts,
            self.status_errors,
            self.flood_sent,
            self.flood_sent + self.flood_remaining,
            self.pace_backoffs,
            self.write_avg_us,
            self.write_max_us,
            self.writes_slow,
            self.writes_in_flight_max,
            self.write_stalls,
            self.answer_to_clear_host_us,
            self.answer_to_clear_host_max_us,
            self.answer_to_clear_wire_us,
            self.answer_to_clear_wire_max_us,
        )
    }
}

/// `status.json`: what the daemon knows, written on every link change and
/// every 30 s (0600, atomic rename) for `deckd status`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Status {
    pub pid: u32,
    pub version: String,
    pub started: Ts,
    pub updated: Ts,
    pub uptime_s: u64,
    pub sid: u32,
    pub demo: bool,
    pub link: Link,
    pub session: SessionState,
    pub fw: Option<String>,
    pub boot: Option<bool>,
    pub face: Option<u8>,
    pub faces: usize,
    /// Slotted agents.
    pub agents: usize,
    pub rev: u64,
    pub err_counts: ErrCounts,
    /// Core→hid channel overflows and the ops refused while one lasted.
    #[serde(default)]
    pub overflows: u32,
    #[serde(default)]
    pub ops_dropped: u64,
    pub metric: Metric,
    /// What the linked deck says about itself (`device.status`); all
    /// `None` while it is not linked or has not said.
    #[serde(default)]
    pub transport: Option<Transport>,
    #[serde(default)]
    pub battery: Option<u8>,
    #[serde(default)]
    pub charging: Option<bool>,
    #[serde(default)]
    pub board: Option<String>,
    /// The delivery record in brief (`crate::deliveries`), counts only, for
    /// `deckd status`; `None` from a daemon that kept none.
    #[serde(default)]
    pub deliveries: Option<crate::deliveries::Brief>,
    /// Whether the collectors run (`crate::dormancy`): a deck linked in the
    /// last ten minutes. `None` in a daemon with no dormancy
    /// (`--no-device`).
    #[serde(default)]
    pub collecting: Option<bool>,
    /// When they stopped: the deck's last link plus the window.
    #[serde(default)]
    pub dormant_since: Option<Ts>,
}

impl Status {
    pub fn read(path: &Path) -> Option<Status> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    pub fn write(&self, path: &Path) -> Result<(), crate::state::StateError> {
        let tmp = path.with_extension("json.tmp");
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        {
            use std::io::Write;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            opts.open(&tmp)?.write_all(&bytes)?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Seconds since `updated`, given the wall clock now.
    pub fn age_s(&self, now: Ts) -> u64 {
        now.millis().saturating_sub(self.updated.millis()) / 1000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_names_every_counter_and_status_round_trips() {
        let m = Metric {
            session: SessionState::Live,
            pace_ms: 8,
            reports_sent: 12,
            err_range: 2,
            err_size: 1,
            flood_sent: 3,
            flood_remaining: 4,
            ..Metric::default()
        };
        let s = m.summary();
        assert!(s.contains("session=Live"));
        assert!(s.contains("reports tx=12"));
        assert!(s.contains("err=3 (size=1 parse=0 range=2 unknown=0)"));
        assert!(s.contains("flood=3/7"));
        assert_eq!(m.err_total(), 3);
        let dir = std::env::temp_dir().join(format!("deckd-metrics-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested/status.json");
        assert!(Status::read(&path).is_none());
        let status = Status {
            pid: 7,
            sid: 9,
            updated: Ts(10_000),
            metric: m,
            deliveries: Some(crate::deliveries::Brief {
                cards: 3,
                answered: 2,
                ..Default::default()
            }),
            ..Status::default()
        };
        status.write(&path).unwrap();
        let back = Status::read(&path).unwrap();
        assert_eq!(back, status);
        assert_eq!(back.age_s(Ts(14_500)), 4);
        assert_eq!(back.age_s(Ts(0)), 0, "saturates");
        // A file from a daemon that kept no delivery record still reads.
        std::fs::write(&path, "{\"pid\": 3, \"future\": true}").unwrap();
        assert_eq!(Status::read(&path).unwrap().pid, 3);
        assert_eq!(Status::read(&path).unwrap().deliveries, None);
        std::fs::write(&path, "nope").unwrap();
        assert!(Status::read(&path).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
