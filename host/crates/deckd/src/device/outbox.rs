//! The outbox (design §6.5): keyed, prioritised (lanes P0..P3), coalescing
//! keep-latest per key, 4/s/slot title token bucket, 1 s aging promotion,
//! device-side shadow diff, barrier mode, 8 ms pacer with the overflow
//! back-off, and the debug title flood. The hid thread calls `enqueue` after folding the op into the
//! `DeviceView` and `next_report` on every pace tick.
//!
//! The outbox never stores a line of its own: an entry is a key plus its
//! age, and the line is taken from the view when it is written, so a `Put`
//! is keep-latest by construction and a barrier dumps what the view holds
//! at the moment each line goes out.
use super::framer::Framer;
use super::view::{BarrierItem, DeviceView, WsGate};
use deck_model::{Control, DeviceOp, DocKind, HostLine, Key, Lane, Slot};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

/// The pace floor (P:39): never faster whatever the config says.
pub const PACE_FLOOR: Duration = Duration::from_millis(8);
/// The overflow-signature back-off pace and how long it lasts.
pub const BACKOFF_PACE: Duration = Duration::from_millis(10);
pub const BACKOFF_FOR: Duration = Duration::from_secs(600);
/// Aging: an entry older than this in P2/P3 is promoted one lane.
pub const AGING: Duration = Duration::from_secs(1);
pub const TITLE_RATE_PER_S: f32 = 4.0;
pub const TITLE_BURST: f32 = 2.0;
/// §7.4's transcript pacing (B16): keep-latest **and ≤4/s**. Only the
/// keep-latest half existed, so a recogniser that emits a partial per
/// buffer — Apple's does, at 10–20 a second — put every one of them on the
/// wire at the 8 ms pace and spent the whole link on a line the operator
/// cannot read that fast anyway. The burst is 2, like a title's: the first
/// word or two of an utterance should appear at once.
pub const TRANSCRIPT_RATE_PER_S: f32 = 4.0;
pub const TRANSCRIPT_BURST: f32 = 2.0;
/// `ws` while the visible face is unknown: at most one per this (§6.4).
pub const WS_UNKNOWN_FACE_EVERY: Duration = Duration::from_secs(2);
/// A burst (for the overflow signature): this many lines within the window.
pub const BURST_LINES: usize = 40;
pub const BURST_WINDOW: Duration = Duration::from_secs(2);

/// What this sid has actually been sent; cleared on every barrier.
#[derive(Debug, Default)]
pub struct Shadow {
    pub sid: u32,
    pub written: HashMap<Key, HostLine>,
}

impl Shadow {
    fn forget_slot(&mut self, slot: Slot) {
        self.written.retain(|k, line| k.slot() != Some(slot)
            && !matches!(line, HostLine::Focus { slot: focused, .. } if *focused == slot.0));
    }
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    since: Instant,
}

/// One paced key's bucket (`Key::AgentTitle(slot)`, `Key::Transcript`).
///
/// Keyed by `Key` rather than by slot (B16) so the transcript has one of
/// its own: it is a control and not a view key, it belongs to no slot's
/// title, and §7.4 gives it its own rate.
#[derive(Debug)]
struct Bucket {
    tokens: f32,
    at: Instant,
    /// Tokens per second and the ceiling, from [`bucket_spec`].
    rate: f32,
    burst: f32,
}

/// The rate and burst of a paced key, or `None` for every key that is not
/// paced at all.
fn bucket_spec(key: Key) -> Option<(f32, f32)> {
    match key {
        Key::AgentTitle(_) => Some((TITLE_RATE_PER_S, TITLE_BURST)),
        Key::Transcript => Some((TRANSCRIPT_RATE_PER_S, TRANSCRIPT_BURST)),
        _ => None,
    }
}

#[derive(Debug)]
struct Barrier {
    items: VecDeque<BarrierItem>,
    /// Keys the barrier has already written (a later `Put` goes to the lanes).
    written: HashSet<Key>,
}

impl Barrier {
    /// True while the barrier still owes a line for `key` (the view will be dumped).
    fn will_dump(&self, key: Key) -> bool {
        if self.written.contains(&key) {
            return false;
        }
        self.items.iter().any(|item| match item {
            BarrierItem::Agent(slot) => {
                matches!(key, Key::AgentState(s) | Key::AgentTitle(s) if s == *slot)
            }
            BarrierItem::Key(k) => *k == key,
            BarrierItem::SetDefault(k, _) => key == Key::Set(*k),
            BarrierItem::Gone(_) | BarrierItem::Synced => false,
        })
    }
}

/// The line whose reports sit in the framer: re-queued by `clear_link`, so
/// a host-side write failure never loses it (the device answers a reopen
/// with our sid and no barrier repairs the lanes, §12).
#[derive(Clone, Debug)]
enum InFlight {
    Key(Key),
    Control(Control),
}

/// The debug title flood (`deckd device flood N`): spinner titles for slot 3
/// at the pace whenever nothing else is due, bypassing the bucket and shadow.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flood {
    pub sent: u64,
    pub remaining: u64,
}

/// Queue depths for `metrics.get`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Depth {
    pub p0: u32,
    pub p1: u32,
    pub p2: u32,
    pub p3: u32,
    pub barrier: u32,
    /// Reports of the line in flight still to write.
    pub reports: u32,
}

impl Depth {
    pub fn total(&self) -> u32 {
        self.p0 + self.p1 + self.p2 + self.p3 + self.barrier
    }
}

#[derive(Debug)]
pub struct Outbox {
    base_pace: Duration,
    backoff_until: Option<Instant>,
    next_write_at: Option<Instant>,
    entries: BTreeMap<Key, Entry>,
    controls: VecDeque<Control>,
    buckets: HashMap<Key, Bucket>,
    shadow: Shadow,
    barrier: Option<Barrier>,
    barrier_done: bool,
    /// Controls only (before the device answered a hello, or while degraded).
    hold_lanes: bool,
    framer: Framer,
    in_flight: Option<InFlight>,
    flood: Flood,
    clock_local: bool,
    hm_override: Option<String>,
    ws_sent_at: Option<Instant>,
    reports_sent: u64,
    lines_sent: u64,
    encode_errors: u64,
    recent_lines: VecDeque<Instant>,
}

impl Outbox {
    pub fn new(pace: Duration, sid: u32) -> Self {
        Outbox {
            base_pace: pace.max(PACE_FLOOR),
            backoff_until: None,
            next_write_at: None,
            entries: BTreeMap::new(),
            controls: VecDeque::new(),
            buckets: HashMap::new(),
            shadow: Shadow {
                sid,
                written: HashMap::new(),
            },
            barrier: None,
            barrier_done: false,
            hold_lanes: false,
            framer: Framer::new(),
            in_flight: None,
            flood: Flood::default(),
            clock_local: true,
            hm_override: None,
            ws_sent_at: None,
            reports_sent: 0,
            lines_sent: 0,
            encode_errors: 0,
            recent_lines: VecDeque::new(),
        }
    }

    /// The configured pace (floor 8 ms), ignoring a back-off.
    pub fn pace(&self) -> Duration {
        self.base_pace
    }

    pub fn set_pace(&mut self, pace: Duration) {
        self.base_pace = pace.max(PACE_FLOOR);
    }

    /// The pace in force at `now`.
    pub fn pace_at(&self, now: Instant) -> Duration {
        match self.backoff_until {
            Some(until) if now < until => BACKOFF_PACE.max(self.base_pace),
            _ => self.base_pace,
        }
    }

    /// Overflow signature seen: 10 ms for 10 minutes (§6.2).
    pub fn backoff(&mut self, now: Instant) {
        self.backoff_until = Some(now + BACKOFF_FOR);
    }

    pub fn in_backoff(&self, now: Instant) -> bool {
        self.backoff_until.is_some_and(|until| now < until)
    }

    /// Time until the next report may go out, `None` when nothing is queued.
    pub fn next_write_in(&self, now: Instant) -> Option<Duration> {
        if self.depth() == 0 && !self.framer.has_pending() && self.flood.remaining == 0 {
            return None;
        }
        Some(
            self.next_write_at
                .map_or(Duration::ZERO, |at| at.saturating_duration_since(now)),
        )
    }

    /// `hello.hm`: the local clock, or a fixed value (dry runs, tests), or none.
    pub fn set_clock(&mut self, local: bool, fixed: Option<String>) {
        self.clock_local = local;
        self.hm_override = fixed;
    }

    /// A new sid: the shadow is empty and a barrier is due.
    pub fn new_sid(&mut self, sid: u32) {
        self.shadow = Shadow {
            sid,
            written: HashMap::new(),
        };
    }

    pub fn sid(&self) -> u32 {
        self.shadow.sid
    }

    pub fn shadow(&self) -> &Shadow {
        &self.shadow
    }

    /// Lines queued in the lanes plus controls plus the barrier's remainder.
    pub fn depth(&self) -> usize {
        self.entries.len()
            + self.controls.len()
            + self.barrier.as_ref().map_or(0, |b| b.items.len())
    }

    /// Depth per lane at `now` (aging applied), for the metrics.
    pub fn depths(&self, now: Instant) -> Depth {
        let mut d = Depth {
            p0: self.controls.len() as u32,
            barrier: self.barrier.as_ref().map_or(0, |b| b.items.len()) as u32,
            reports: self.framer.pending_reports() as u32,
            ..Depth::default()
        };
        for (key, entry) in &self.entries {
            match effective_lane(*key, entry, now) {
                Lane::P0 => d.p0 += 1,
                Lane::P1 => d.p1 += 1,
                Lane::P2 => d.p2 += 1,
                Lane::P3 => d.p3 += 1,
            }
        }
        d
    }

    pub fn reports_sent(&self) -> u64 {
        self.reports_sent
    }

    pub fn lines_sent(&self) -> u64 {
        self.lines_sent
    }

    pub fn encode_errors(&self) -> u64 {
        self.encode_errors
    }

    pub fn in_barrier(&self) -> bool {
        self.barrier.is_some()
    }

    pub fn flood_state(&self) -> Flood {
        self.flood
    }

    /// Queue `n` more flood titles.
    pub fn flood(&mut self, n: u64) {
        self.flood.remaining = self.flood.remaining.saturating_add(n);
    }

    /// The `m` and byte length of the last `n` lines written (redacted).
    pub fn recent(&self, n: usize) -> Vec<(String, usize)> {
        self.framer.recent(n)
    }

    /// True when ≥40 lines went out in the last 2 s (the overflow signature's burst).
    pub fn burst(&mut self, now: Instant) -> bool {
        self.trim_recent(now);
        self.recent_lines.len() >= BURST_LINES
    }

    fn trim_recent(&mut self, now: Instant) {
        while self
            .recent_lines
            .front()
            .is_some_and(|t| now.duration_since(*t) > BURST_WINDOW)
        {
            self.recent_lines.pop_front();
        }
    }

    /// Link dropped: the reports in flight are void (the device drops a
    /// partial line), so the line they carried goes back to its queue, the
    /// pacer restarts on the next open. Lanes and the
    /// flood are kept (the view is desired state; a barrier line is not
    /// re-queued because the next barrier rewrites the device).
    pub fn clear_link(&mut self, now: Instant) {
        self.framer.clear();
        self.next_write_at = None;
        self.barrier = None;
        self.barrier_done = false;
        match self.in_flight.take() {
            Some(InFlight::Key(key)) => {
                self.shadow.written.remove(&key);
                if let Key::DocText(slot, DocKind::Reply | DocKind::Workflow, _) = key {
                    // Its revision envelope may have been in the same batch.
                    self.shadow.written.remove(&Key::Reply(slot));
                }
                self.entries.entry(key).or_insert(Entry { since: now });
            }
            Some(InFlight::Control(control)) => self.controls.push_front(control),
            None => {}
        }
    }

    /// The report `next_report` last returned reached the device: once the
    /// framer is empty the line it belonged to is no longer at risk.
    pub fn report_written(&mut self) {
        if !self.framer.has_pending() {
            self.in_flight = None;
        }
    }

    /// The device itself changed `key` (a `set`/`faces` line): the shadow
    /// learns it so a later identical `Put` is not echoed back (P:102-104).
    pub fn shadow_put(&mut self, key: Key, line: HostLine) {
        self.entries.remove(&key);
        self.shadow.written.insert(key, line);
    }

    /// Queue `op` after the view has folded it in (`view` is the desired
    /// state; the outbox decides what and when to write).
    pub fn enqueue(&mut self, view: &DeviceView, op: DeviceOp, now: Instant) {
        match op {
            DeviceOp::Put(key, _) => self.put(key, now),
            DeviceOp::Remove(key) => self.remove(key, now),
            DeviceOp::Sync { boot, .. } => self.begin_barrier(view, boot),
            DeviceOp::Control(control) => self.control(control),
        }
    }

    fn put(&mut self, key: Key, now: Instant) {
        if !key.in_barrier() {
            // Hello, CardClear, Transcript, Name and AgentGone are controls, never view keys.
            return;
        }
        if self.barrier.as_ref().is_some_and(|b| b.will_dump(key)) {
            // The barrier dumps the view's latest for this key when it gets there.
            return;
        }
        // A live document can change revision repeatedly while HID is busy.
        // Keep only its newest window, retaining the oldest enqueue time so
        // updates cannot postpone it forever. An in-flight frame may finish.
        let mut since = now;
        if let Key::DocText(slot, doc, _) = key {
            self.entries.retain(|old, entry| {
                if *old != key && super::view::same_document(slot, doc, *old) {
                    since = since.min(entry.since);
                    false
                } else { true }
            });
            self.shadow.written.retain(|old, _| *old == key || !super::view::same_document(slot, doc, *old));
        }
        // Keep-latest: the entry keeps its age, the view holds the content.
        self.entries.entry(key).or_insert(Entry { since });
    }

    fn remove(&mut self, key: Key, now: Instant) {
        match key {
            Key::AgentState(slot) | Key::AgentGone(slot) => {
                self.entries.retain(|k, _| k.slot() != Some(slot));
                let device_has = self.shadow.written.keys().any(|k| k.slot() == Some(slot))
                    || self
                        .barrier
                        .as_ref()
                        .is_some_and(|b| b.written.iter().any(|k| k.slot() == Some(slot)));
                if device_has {
                    self.entries
                        .entry(Key::AgentGone(slot))
                        .or_insert(Entry { since: now });
                }
            }
            other => {
                self.entries.remove(&other);
                // The core withdrew the key (a device `channel` took the
                // focus, a card was cleared): the shadow may not claim it, or
                // an identical later `Put` would be suppressed as "on the device".
                self.shadow.written.remove(&other);
            }
        }
    }

    /// A one-shot line (hello, focus{ack}, card clear, transcript, name).
    pub fn control(&mut self, control: Control) {
        match &control {
            Control::Hello | Control::Synced => {
                if self.controls.contains(&control) {
                    return;
                }
            }
            Control::Focus { slot, ack } => {
                // Keep-latest, and the highest ack wins (the device drops older acks).
                if let Some(at) = self
                    .controls
                    .iter()
                    .position(|c| matches!(c, Control::Focus { .. }))
                {
                    let keep_old = match (&self.controls[at], ack) {
                        (Control::Focus { ack: Some(old), .. }, Some(new)) => *old > *new,
                        (Control::Focus { ack: Some(_), .. }, None) => true,
                        _ => false,
                    };
                    if keep_old {
                        return;
                    }
                    self.controls[at] = Control::Focus {
                        slot: *slot,
                        ack: *ack,
                    };
                    return;
                }
            }
            Control::CardClear(slot, id) => {
                let key = Key::Card(*slot, *id);
                self.entries.remove(&key);
                self.entries
                    .remove(&Key::DocText(*slot, DocKind::Card, *id));
                if !self.shadow.written.contains_key(&key) || self.controls.contains(&control) {
                    // The device never saw the card under this sid: nothing to clear.
                    return;
                }
            }
            Control::Transcript { press, .. } => {
                // A completed recording must survive a partial from the next
                // press. Coalesce only previews of this same recording.
                self.controls.retain(|c| !matches!(c,
                    Control::Transcript { press: old, last: false, .. } if old == press));
            }
            Control::Image(_) => { self.controls.retain(|c| !matches!(c, Control::Image(_))); }
            Control::SessionMenu(_) => {
                self.controls.retain(|c| !matches!(c, Control::SessionMenu(_)));
            }
            Control::Name(_) => {
                self.controls.retain(|c| !matches!(c, Control::Name(_)));
            }
        }
        self.controls.push_back(control);
    }

    /// Switch to barrier mode: the ordered dump of `view` then `synced`. The
    /// lanes empty into the view, the shadow starts over (everything is
    /// re-sent), the buckets refill.
    pub fn begin_barrier(&mut self, view: &DeviceView, boot: bool) {
        // A same-sid barrier purges nothing on the device (only a stale
        // roster is dropped at `synced`), so a pending `gone` leads it.
        let mut items: VecDeque<BarrierItem> = self
            .entries
            .keys()
            .filter_map(|k| match k {
                Key::AgentGone(slot) => Some(BarrierItem::Gone(*slot)),
                _ => None,
            })
            .collect();
        items.extend(view.barrier_plan(boot));
        self.barrier = Some(Barrier {
            items,
            written: HashSet::new(),
        });
        self.barrier_done = false;
        self.entries.clear();
        self.shadow.written.clear();
        self.buckets.clear();
        self.ws_sent_at = None;
    }

    /// True once, after the barrier's `synced` went into the framer.
    pub fn take_barrier_done(&mut self) -> bool {
        std::mem::take(&mut self.barrier_done)
    }

    /// While held, only controls (hello, acks, clears) are written: the
    /// session is not live, so the barrier or the lanes wait (§6.3).
    pub fn hold_lanes(&mut self, hold: bool) {
        self.hold_lanes = hold;
    }

    pub fn lanes_held(&self) -> bool {
        self.hold_lanes
    }

    /// The visible face or the strip changed: `ws`/`macro` entries the shadow
    /// lacks become due (gating is re-evaluated at write time).
    pub fn reconsider(&mut self, view: &DeviceView, now: Instant) {
        for (key, line) in view.entries() {
            if matches!(key, Key::Ws | Key::Macro(_)) && self.shadow.written.get(key) != Some(line)
            {
                self.put(*key, now);
            }
        }
    }

    /// The next 64-byte report to write, if the pacer allows one now.
    pub fn next_report(&mut self, view: &DeviceView, now: Instant) -> Option<[u8; 64]> {
        if self.next_write_at.is_some_and(|at| now < at) {
            return None;
        }
        let pace = self.pace_at(now);
        if !self.framer.has_pending() {
            loop {
                let (line, origin) = self.next_line(view, now)?;
                // An aged document can outrank its newly queued revision
                // envelope. Freeze the pair in the framer so a stream of newer
                // revisions cannot leave every window rejected by the device.
                if let HostLine::DocText { slot, doc: DocKind::Reply | DocKind::Workflow, id, .. } = &line {
                    let parent = Key::Reply(Slot(*slot));
                    if let Some(envelope @ HostLine::Reply { rev, .. }) = view.get(&parent) {
                        if rev == id && self.shadow.written.get(&parent) != Some(envelope) {
                            match self.framer.push_line(envelope) {
                                Ok(_) => {
                                    self.shadow.written.insert(parent, envelope.clone());
                                    self.entries.remove(&parent);
                                    self.lines_sent += 1;
                                    self.recent_lines.push_back(now);
                                }
                                Err(e) => {
                                    self.encode_errors += 1;
                                    tracing::warn!(error = %e, "document revision dropped: cannot encode");
                                    continue;
                                }
                            }
                        }
                    }
                }
                match self.framer.push_line(&line) {
                    Ok(_) => {
                        self.in_flight = origin;
                        self.lines_sent += 1;
                        self.recent_lines.push_back(now);
                        self.trim_recent(now);
                        break;
                    }
                    Err(e) => {
                        // A line over the caps is a bug, never sent (§6.5).
                        self.encode_errors += 1;
                        tracing::warn!(m = line.m(), error = %e, "line dropped: cannot encode");
                    }
                }
            }
        }
        let report = self.framer.next_report()?;
        self.reports_sent += 1;
        self.next_write_at = Some(now + pace);
        Some(report)
    }

    /// The next whole line, strict priority: controls, then the barrier,
    /// then lanes P0..P3 with aging, then the flood; with what to re-queue
    /// should the link drop under it.
    fn next_line(
        &mut self,
        view: &DeviceView,
        now: Instant,
    ) -> Option<(HostLine, Option<InFlight>)> {
        if let Some(control) = self.take_control(now) {
            match control {
                // The card leaves the device with the clear (no leak per card
                // ever shown, and a re-Put of the id is a real send again).
                Control::CardClear(slot, id) => {
                    self.shadow.written.remove(&Key::Card(slot, id));
                    self.shadow
                        .written
                        .remove(&Key::DocText(slot, DocKind::Card, id));
                }
                // The device moved the selection: the last PC focus is stale.
                Control::Focus { ack: Some(_), .. } => {
                    self.shadow.written.remove(&Key::Focus);
                }
                _ => {}
            }
            let line = self.control_line(control.clone());
            return Some((line, Some(InFlight::Control(control))));
        }
        if self.hold_lanes {
            return None;
        }
        if self.barrier.is_some() {
            if let Some(line) = self.next_barrier_line(view) {
                return Some((line, None));
            }
        }
        if let Some((key, line)) = self.next_lane_line(view, now) {
            return Some((line, Some(InFlight::Key(key))));
        }
        if self.flood.remaining > 0 {
            let i = self.flood.sent;
            self.flood.sent += 1;
            self.flood.remaining -= 1;
            return Some((crate::demo::flood_title(i as usize), None));
        }
        None
    }

    fn next_barrier_line(&mut self, view: &DeviceView) -> Option<HostLine> {
        let barrier = self.barrier.as_mut()?;
        while let Some(item) = barrier.items.pop_front() {
            match item {
                BarrierItem::Gone(slot) => {
                    self.entries.remove(&Key::AgentGone(slot));
                    self.shadow.forget_slot(slot);
                    self.buckets.remove(&Key::AgentTitle(slot));
                    return Some(HostLine::AgentGone { slot: slot.0 });
                }
                BarrierItem::Agent(slot) => {
                    let Some(line) = view.agent_line(slot) else {
                        continue;
                    };
                    barrier.written.insert(Key::AgentState(slot));
                    barrier.written.insert(Key::AgentTitle(slot));
                    for key in [Key::AgentState(slot), Key::AgentTitle(slot)] {
                        if let Some(l) = view.get(&key) {
                            self.shadow.written.insert(key, l.clone());
                        }
                    }
                    return Some(line);
                }
                BarrierItem::Key(key) => {
                    let Some(line) = view.get(&key) else {
                        continue;
                    };
                    barrier.written.insert(key);
                    self.shadow.written.insert(key, line.clone());
                    if key == Key::Ws {
                        self.ws_sent_at = None;
                    }
                    return Some(line.clone());
                }
                BarrierItem::SetDefault(key, value) => {
                    let line = HostLine::Set { key, value };
                    barrier.written.insert(Key::Set(key));
                    self.shadow.written.insert(Key::Set(key), line.clone());
                    return Some(line);
                }
                BarrierItem::Synced => break,
            }
        }
        // `synced` is always last (P:86-89).
        self.barrier = None;
        self.barrier_done = true;
        Some(HostLine::Synced)
    }

    fn next_lane_line(&mut self, view: &DeviceView, now: Instant) -> Option<(Key, HostLine)> {
        // Rank: lane, age, then a pending `gone` before anything else at
        // the same age (one core apply reassigns a slot in one batch).
        let mut best: Option<((Lane, Instant, bool), Key)> = None;
        let mut drop = Vec::new();
        let gone: Vec<Slot> = self
            .entries
            .keys()
            .filter_map(|k| match k {
                Key::AgentGone(slot) => Some(*slot),
                _ => None,
            })
            .collect();
        for (key, entry) in &self.entries {
            let is_gone = matches!(key, Key::AgentGone(_));
            if !is_gone && key.slot().is_some_and(|s| gone.contains(&s)) {
                // The slot's `gone` goes first: its new lines must not land
                // on the old agent as a diff, nor be deleted with it (§6.5).
                continue;
            }
            if let (Key::Focus, Some(HostLine::Focus { slot, .. })) = (key, view.get(key)) {
                let slot = Slot(*slot);
                // Focus is P0, but it depends on its target's P1 lifecycle.
                // Sending it before `gone` + `agent` focuses the predecessor
                // (then returns to the roster) or an unknown slot (ignored).
                if gone.contains(&slot)
                    || (self.entries.contains_key(&Key::AgentState(slot))
                        && self.agent_diff(view, Key::AgentState(slot)).is_some())
                {
                    continue;
                }
            }
            let lane = effective_lane(*key, entry, now);
            let candidate = match key {
                Key::AgentTitle(_) => {
                    if view.get(key) == self.shadow.written.get(key) {
                        drop.push(*key);
                        false
                    } else {
                        self.bucket_allows(*key, now)
                    }
                }
                Key::AgentState(_) => match self.agent_diff(view, *key) {
                    Some(_) => true,
                    None => {
                        drop.push(*key);
                        false
                    }
                },
                Key::Ws => match view.ws_gate() {
                    WsGate::Visible => true,
                    WsGate::Unknown => self
                        .ws_sent_at
                        .is_none_or(|t| now.duration_since(t) >= WS_UNKNOWN_FACE_EVERY),
                    WsGate::No => false,
                },
                Key::System => view.system_allowed(),
                Key::Macro(face) => view.macro_allowed(*face),
                Key::Set(key) => view.set_allowed(*key),
                Key::AgentGone(_) => true,
                _ => true,
            };
            if !candidate {
                continue;
            }
            if !matches!(
                key,
                Key::AgentGone(_) | Key::AgentState(_) | Key::AgentTitle(_)
            ) && view.get(key) == self.shadow.written.get(key)
            {
                // Already on the device (or gone from the view): nothing to send.
                drop.push(*key);
                continue;
            }
            let rank = (lane, entry.since, !is_gone);
            if best.is_none_or(|(r, _)| rank < r) {
                best = Some((rank, *key));
            }
        }
        for key in drop {
            self.entries.remove(&key);
        }
        let (_, key) = best?;
        self.entries.remove(&key);
        let line = match key {
            Key::AgentTitle(_) => {
                self.take_token(key, now);
                let line = view.get(&key)?.clone();
                self.shadow.written.insert(key, line.clone());
                line
            }
            Key::AgentState(_) => {
                let line = self.agent_diff(view, key)?;
                self.shadow.written.insert(key, view.get(&key)?.clone());
                line
            }
            Key::AgentGone(slot) => {
                self.shadow.forget_slot(slot);
                self.buckets.remove(&Key::AgentTitle(slot));
                HostLine::AgentGone { slot: slot.0 }
            }
            other => {
                let line = view.get(&other)?.clone();
                if other == Key::Ws {
                    self.ws_sent_at = Some(now);
                }
                self.shadow.written.insert(other, line.clone());
                line
            }
        };
        Some((key, line))
    }

    /// The `agent` line for `key`: the full record when the shadow has none
    /// for the slot under this sid, else only the changed fields (P:55-57);
    /// `None` when nothing changed.
    fn agent_diff(&self, view: &DeviceView, key: Key) -> Option<HostLine> {
        let HostLine::Agent {
            slot,
            kind,
            name,
            project,
            state,
            since_s,
            ..
        } = view.get(&key)?
        else {
            return None;
        };
        let Some(HostLine::Agent {
            kind: k0,
            name: n0,
            project: p0,
            state: s0,
            since_s: a0,
            ..
        }) = self.shadow.written.get(&key)
        else {
            return Some(HostLine::Agent {
                slot: *slot,
                kind: *kind,
                name: name.clone(),
                project: project.clone(),
                state: *state,
                title: None,
                since_s: *since_s,
            });
        };
        let diff = HostLine::Agent {
            slot: *slot,
            kind: kind.filter(|k| Some(*k) != *k0),
            name: name.clone().filter(|n| Some(n) != n0.as_ref()),
            project: project.clone().filter(|p| Some(p) != p0.as_ref()),
            state: state.filter(|s| Some(*s) != *s0),
            title: None,
            since_s: since_s.filter(|a| Some(*a) != *a0),
        };
        match &diff {
            HostLine::Agent {
                kind: None,
                name: None,
                project: None,
                state: None,
                since_s: None,
                ..
            } => None,
            _ => Some(diff),
        }
    }

    fn control_line(&self, control: Control) -> HostLine {
        match control {
            Control::Hello => HostLine::Hello {
                sid: self.shadow.sid,
                hm: self.hm_override.clone().or_else(|| {
                    self.clock_local
                        .then(|| super::session::local_hm(std::time::SystemTime::now()))
                        .flatten()
                }),
            },
            Control::CardClear(slot, id) => HostLine::CardClear { slot: slot.0, id },
            Control::Focus { slot, ack } => HostLine::Focus { slot: slot.0, ack },
            Control::Transcript { slot, text, last, press, failed, channel_tip } => HostLine::Transcript {
                slot: slot.0,
                text,
                last, press, failed, channel_tip,
            },
            Control::Image(line) => line,
            Control::SessionMenu(menu) => HostLine::SessionMenu(menu),
            Control::Name(text) => HostLine::Name { text },
            Control::Synced => HostLine::Synced,
        }
    }

    /// The next control to write, skipping a **partial** transcript whose
    /// token bucket is empty (B16).
    ///
    /// §7.4 asks for "keep-latest, ≤4/s, last 120 B" and only the
    /// keep-latest half existed: `control` replaces a queued transcript
    /// with the newer one, so nothing piles up, but nothing limited the
    /// *rate* either. Apple's recogniser emits a partial per buffer —
    /// 10–20 a second — and every one of them went out at the 8 ms pace,
    /// spending the link on a line no operator can read that fast.
    ///
    /// The held-back transcript stays queued (a later partial replaces it),
    /// and a **final** (`last:true`) bypasses the bucket: it is one line
    /// per session, it is the line that matters, and making the operator
    /// wait 250 ms to see what was heard is the opposite of the point.
    /// Every other control is untouched — a `card clear` behind a paced
    /// transcript would be a delivery the deck never confirmed.
    fn take_control(&mut self, now: Instant) -> Option<Control> {
        let i = self.controls.iter().position(|c| match c {
            Control::Transcript { last: false, .. } => self.bucket_allows(Key::Transcript, now),
            _ => true,
        })?;
        let control = self.controls.remove(i)?;
        if matches!(control, Control::Transcript { last: false, .. }) {
            self.take_token(Key::Transcript, now);
        }
        Some(control)
    }

    /// Whether `key`'s bucket has a token right now (unpaced keys always
    /// do).
    fn bucket_allows(&self, key: Key, now: Instant) -> bool {
        self.buckets.get(&key).is_none_or(|b| refill(b, now) >= 1.0)
    }

    fn take_token(&mut self, key: Key, now: Instant) {
        let Some((rate, burst)) = bucket_spec(key) else {
            return;
        };
        let bucket = self.buckets.entry(key).or_insert(Bucket {
            tokens: burst,
            at: now,
            rate,
            burst,
        });
        bucket.tokens = (refill(bucket, now) - 1.0).max(0.0);
        bucket.at = now;
    }
}

fn effective_lane(key: Key, entry: &Entry, now: Instant) -> Lane {
    let lane = key.lane();
    if matches!(lane, Lane::P2 | Lane::P3) && now.duration_since(entry.since) >= AGING {
        match lane {
            Lane::P2 => Lane::P1,
            _ => Lane::P2,
        }
    } else {
        lane
    }
}

fn refill(b: &Bucket, now: Instant) -> f32 {
    (b.tokens + now.duration_since(b.at).as_secs_f32() * b.rate).min(b.burst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_windows_keep_their_revision_envelope_during_a_live_stream() {
        let now = Instant::now();
        let mut outbox = Outbox::new(PACE_FLOOR, 1);
        let mut view = DeviceView::new();
        let mut partial = Vec::new();
        let mut device_rev = 0;
        let mut windows = 0;
        for rev in 1..=1000 {
            let at = now + PACE_FLOOR * u32::from(rev);
            put(&mut outbox, &mut view, Key::Reply(Slot(1)), HostLine::Reply {
                slot: 1, text: "Workflow".into(), rev,
            }, at);
            put(&mut outbox, &mut view, Key::DocText(Slot(1), DocKind::Workflow, rev), HostLine::DocText {
                slot: 1, doc: DocKind::Workflow, id: rev, first: 0, total: 10,
                font: deck_proto::DocFont::RichRow, lines: vec!["p0A live workflow step".into(); 10],
            }, at);
            if let Some(report) = outbox.next_report(&view, at) {
                partial.extend_from_slice(&report[3..3 + report[2] as usize]);
                if partial.last() == Some(&b'\n') {
                    match deck_proto::parse_host_line(&partial[..partial.len() - 1]).unwrap() {
                        HostLine::Reply { rev, .. } => device_rev = rev,
                        HostLine::DocText { id, .. } => {
                            assert_eq!(id, device_rev, "the firmware would reject an unpaired window");
                            windows += 1;
                        }
                        _ => panic!("unexpected stream line"),
                    }
                    partial.clear();
                }
                outbox.report_written();
            }
        }
        assert!(windows > 50, "continuous updates must not starve the document");
        assert!(view.len() <= 2);
        assert!(outbox.entries.len() <= 2);

        // A failure part way through a frozen pair must resend its envelope.
        outbox.clear_link(now + Duration::from_secs(9));
        partial.clear();
        let lines = drain(&mut outbox, &view, now + Duration::from_secs(10));
        for line in lines {
            match deck_proto::parse_host_line(line.as_bytes()).unwrap() {
                HostLine::Reply { rev, .. } => device_rev = rev,
                HostLine::DocText { id, .. } => assert_eq!(id, device_rev),
                _ => panic!("unexpected stream line"),
            }
        }
        assert_eq!(device_rev, 1000);
    }

    #[test]
    fn workflow_revision_flood_keeps_one_document_and_its_original_queue_age() {
        let now = Instant::now();
        let mut outbox = Outbox::new(PACE_FLOOR, 1);
        let mut view = DeviceView::new();
        for rev in 1..=1000 {
            let doc = if rev % 2 == 0 { DocKind::Workflow } else { DocKind::Reply };
            let key = Key::DocText(Slot(1), doc, rev);
            let line = HostLine::DocText { slot: 1, doc, id: rev, first: 0, total: 1,
                font: deck_proto::DocFont::RichRow, lines: vec!["p0Latest step".into()] };
            put(&mut outbox, &mut view, key, line, now + Duration::from_millis(rev as u64));
            assert_eq!(view.len(), 1);
            assert_eq!(outbox.entries.len(), 1);
            assert_eq!(outbox.entries[&key].since, now + Duration::from_millis(1));
        }
        assert!(view.get(&Key::DocText(Slot(1), DocKind::Workflow, 1000)).is_some());
    }

    use deck_model::{AgentState, FaceEntry, FaceId, Kind, SetKey, SyncReason, Template};

    fn title(slot: u8, t: &str) -> HostLine {
        HostLine::Agent {
            slot,
            kind: None,
            name: None,
            project: None,
            state: None,
            title: Some(t.into()),
            since_s: None,
        }
    }

    fn agent(slot: u8, state: AgentState, since: Option<u32>) -> HostLine {
        HostLine::Agent {
            slot,
            kind: Some(Kind::Claude),
            name: Some("web".into()),
            project: None,
            state: Some(state),
            title: None,
            since_s: since,
        }
    }

    fn reply(slot: u8, text: &str) -> HostLine {
        HostLine::Reply {
            slot,
            text: text.into(),
            rev: 1,
        }
    }

    fn card(slot: u8, id: u16) -> HostLine {
        HostLine::Card {
            slot,
            id,
            kind: deck_model::CardKind::Q,
            text: "t".into(),
            choices: vec![],
            rec: None,
        }
    }

    fn put(o: &mut Outbox, v: &mut DeviceView, key: Key, line: HostLine, now: Instant) {
        let op = DeviceOp::Put(key, line);
        v.apply(&op);
        o.enqueue(v, op, now);
    }

    fn text(r: &[u8; 64]) -> String {
        String::from_utf8_lossy(&r[3..3 + r[2] as usize]).into_owned()
    }

    /// Every line the outbox writes from `t0`, at the pace, until it is
    /// idle; a line spanning several reports is reassembled.
    fn drain(o: &mut Outbox, v: &DeviceView, t0: Instant) -> Vec<String> {
        let mut lines = Vec::new();
        let mut partial = Vec::new();
        let mut t = t0;
        let mut idle = 0;
        while idle < 3 {
            match o.next_report(v, t) {
                Some(r) => {
                    idle = 0;
                    o.report_written();
                    let n = r[2] as usize;
                    partial.extend_from_slice(&r[3..3 + n]);
                    if partial.last() == Some(&b'\n') {
                        partial.pop();
                        lines.push(String::from_utf8_lossy(&partial).into_owned());
                        partial.clear();
                    }
                }
                None => idle += 1,
            }
            t += Duration::from_millis(8);
        }
        lines
    }

    #[test]
    fn system_samples_coalesce_and_heartbeats_survive_shadow_deduplication() {
        let now = Instant::now();
        let mut o = Outbox::new(PACE_FLOOR, 1);
        let mut v = DeviceView::new();
        let sample = |seq| HostLine::System(deck_proto::SystemStatus {
            cpu: Some(25), sample: seq, ..deck_proto::SystemStatus::EMPTY
        });
        put(&mut o, &mut v, Key::System, sample(1), now);
        assert!(drain(&mut o, &v, now).is_empty(), "legacy deck gets no unknown messages");
        v.set_faces(vec![FaceEntry { f: 7, t: Template::System, n: "System".into() }]);
        for seq in 2..=100 {
            put(&mut o, &mut v, Key::System, sample(seq), now);
        }
        assert_eq!(v.len(), 1);
        assert_eq!(o.entries.len(), 1);
        let lines = drain(&mut o, &v, now + Duration::from_secs(2));
        assert_eq!(lines.len(), 1);
        assert_eq!(deck_proto::parse_host_line(lines[0].as_bytes()).unwrap(), sample(100));
        put(&mut o, &mut v, Key::System, sample(101), now + Duration::from_secs(4));
        let lines = drain(&mut o, &v, now + Duration::from_secs(4));
        assert_eq!(lines.len(), 1, "identical readings still renew the deck's freshness timer");
        assert_eq!(deck_proto::parse_host_line(lines[0].as_bytes()).unwrap(), sample(101));
    }

    #[test]
    fn the_pacer_spaces_reports_by_at_least_eight_ms() {
        let mut o = Outbox::new(Duration::from_millis(4), 1);
        assert_eq!(o.pace(), Duration::from_millis(8), "floor");
        let mut view = DeviceView::new();
        let t0 = Instant::now();
        put(
            &mut o,
            &mut view,
            Key::Reply(Slot(1)),
            reply(1, &"a".repeat(100)),
            t0,
        );
        assert_eq!(o.depth(), 1);
        assert!(o.next_report(&view, t0).is_some());
        assert!(o
            .next_report(&view, t0 + Duration::from_millis(7))
            .is_none());
        assert!(o
            .next_report(&view, t0 + Duration::from_millis(8))
            .is_some());
        assert!(o
            .next_report(&view, t0 + Duration::from_millis(16))
            .is_some());
        assert!(
            o.next_report(&view, t0 + Duration::from_millis(24))
                .is_none(),
            "line done"
        );
        assert_eq!(o.reports_sent(), 3);
        assert_eq!(o.lines_sent(), 1);
        assert_eq!(o.depth(), 0);
        // The back-off raises the pace to 10 ms for ten minutes — and the
        // numbers below are literals on purpose. Written as
        // `pace_at(t0 + BACKOFF_FOR)` this row holds for a back-off of *any*
        // length: §12's row promises ten minutes, so ten minutes is what has
        // to be asserted. (Measured: `BACKOFF_FOR` could be cut to 60 s with
        // the whole suite still green.)
        o.backoff(t0);
        assert_eq!(o.pace_at(t0), Duration::from_millis(10));
        assert!(o.in_backoff(t0));
        assert!(
            o.in_backoff(t0 + Duration::from_secs(9 * 60)),
            "still paced nine minutes in"
        );
        assert_eq!(
            o.pace_at(t0 + Duration::from_secs(9 * 60)),
            Duration::from_millis(10)
        );
        assert!(
            !o.in_backoff(t0 + Duration::from_secs(11 * 60)),
            "and over by eleven"
        );
        assert_eq!(
            o.pace_at(t0 + Duration::from_secs(11 * 60)),
            Duration::from_millis(8)
        );
        o.set_pace(Duration::from_millis(12));
        assert_eq!(
            o.pace_at(t0),
            Duration::from_millis(12),
            "never faster than configured"
        );
    }

    #[test]
    fn controls_go_before_the_barrier_which_ends_in_synced() {
        let mut o = Outbox::new(Duration::from_millis(8), 1);
        let mut view = DeviceView::new();
        view.apply(&DeviceOp::Put(Key::AgentTitle(Slot(1)), title(1, "t")));
        let t0 = Instant::now();
        o.enqueue(
            &view,
            DeviceOp::Sync {
                boot: false,
                reason: SyncReason::Start,
            },
            t0,
        );
        assert!(o.in_barrier());
        o.control(Control::Focus {
            slot: Slot(1),
            ack: Some(3),
        });
        assert_eq!(
            drain(&mut o, &view, t0),
            [
                "{\"m\":\"focus\",\"agent\":1,\"ack\":3}",
                "{\"m\":\"agent\",\"id\":1,\"title\":\"t\"}",
                "{\"m\":\"synced\"}"
            ]
        );
        assert!(!o.in_barrier());
        assert!(o.take_barrier_done());
        assert!(!o.take_barrier_done(), "reported once");
    }

    /// B16 / §7.4: the transcript is keep-latest **and ≤4/s**.
    ///
    /// Only the keep-latest half existed, and it is not the half that
    /// matters here: `control` already replaced a queued partial with the
    /// newer one, so nothing piled up — but nothing limited the rate
    /// either, and Apple's recogniser emits a partial per buffer, 10–20 a
    /// second. Every one of them went out at the 8 ms pace, spending the
    /// link on a line no operator can read that fast, while the card and
    /// the state lines behind it waited.
    #[test]
    fn the_transcript_is_keep_latest_and_paced_and_the_final_is_not_held() {
        let mut o = Outbox::new(Duration::from_millis(8), 1);
        let mut view = DeviceView::new();
        let t0 = Instant::now();
        let partial = |n: usize| Control::Transcript {
            slot: Slot(1),
            text: format!("word {n}"),
            last: false, press: None, failed: false, channel_tip: false,
        };
        // Keep-latest: ten partials in one apply are one line, the newest.
        for n in 0..10 {
            o.control(partial(n));
        }
        assert_eq!(o.depth(), 1, "keep-latest");
        let r = o.next_report(&view, t0).expect("the newest partial");
        assert!(text(&r).contains("word 9"), "{}", text(&r));
        o.report_written();

        // The burst is 2: one more goes at once, then the bucket is empty.
        o.control(partial(10));
        let r = o
            .next_report(&view, t0 + Duration::from_millis(8))
            .expect("the burst");
        assert!(text(&r).contains("word 10"));
        o.report_written();
        o.control(partial(11));
        // Work behind it is **not** held back with it: only the
        // transcript is paced, and everything else the deck is owed would
        // be a much worse thing to delay than a partial.
        put(
            &mut o,
            &mut view,
            Key::Set(SetKey::Mic),
            HostLine::Set {
                key: SetKey::Mic,
                value: 2,
            },
            t0 + Duration::from_millis(16),
        );
        let r = o
            .next_report(&view, t0 + Duration::from_millis(16))
            .expect("the other line, not the throttled partial");
        assert!(text(&r).contains("\"m\":\"set\""), "{}", text(&r));
        o.report_written();
        assert_eq!(o.depth(), 1, "the partial is still owed, not dropped");

        // A quarter of a second later the bucket has a token again — and
        // what goes out is the **latest** partial, not the one that was
        // held.
        o.control(partial(12));
        let r = o
            .next_report(&view, t0 + Duration::from_millis(280))
            .expect("the refilled bucket");
        assert!(text(&r).contains("word 12"), "{}", text(&r));
        o.report_written();

        // The final is never held: it is one line per session, it is the
        // line that matters, and 250 ms of "what did it hear?" is exactly
        // the wait this pacing exists to avoid.
        o.control(Control::Transcript {
            slot: Slot(1),
            text: "done".into(),
            last: true, press: None, failed: false, channel_tip: false,
        });
        let r = o
            .next_report(&view, t0 + Duration::from_millis(288))
            .expect("the final goes at once, with the bucket still empty");
        let line = text(&r);
        assert!(line.contains("\"text\":\"done\""), "{line}");
        assert!(line.contains("\"final\":1"), "{line}");
        o.report_written();

        // The title bucket is a different bucket: throttling transcripts
        // must not throttle titles, and a slot's `gone` clears only its
        // own.
        assert!(o.bucket_allows(Key::AgentTitle(Slot(1)), t0 + Duration::from_millis(288)));
        assert!(!o.bucket_allows(Key::Transcript, t0 + Duration::from_millis(288)));
    }

    #[test]
    fn a_new_preview_cannot_erase_an_earlier_completion() {
        let mut outbox = Outbox::new(Duration::from_millis(8), 1);
        let view = DeviceView::new();
        for (press, last, text) in [(1, true, "first final"), (2, false, "preview"), (2, false, "new preview"), (2, true, "second final")] {
            outbox.control(Control::Transcript { slot: Slot(1), text: text.into(), last, press: Some(press), failed: false, channel_tip: false });
        }
        let lines = drain(&mut outbox, &view, Instant::now());
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("first final") && lines[0].contains("\"p\":1"));
        assert!(lines[1].contains("second final") && lines[1].contains("\"p\":2"));
    }

    #[test]
    fn keep_latest_and_the_title_bucket() {
        let mut o = Outbox::new(Duration::from_millis(8), 1);
        let mut view = DeviceView::new();
        let t0 = Instant::now();
        for i in 0..10 {
            put(
                &mut o,
                &mut view,
                Key::AgentTitle(Slot(3)),
                title(3, &i.to_string()),
                t0,
            );
        }
        assert_eq!(o.depth(), 1, "keep-latest");
        let r = o.next_report(&view, t0).unwrap();
        assert!(text(&r).contains("\"title\":\"9\""));
        assert_eq!(o.shadow().written.len(), 1);
        // Burst of 2, then the bucket holds titles back while a reply flows.
        put(
            &mut o,
            &mut view,
            Key::AgentTitle(Slot(3)),
            title(3, "x"),
            t0,
        );
        assert!(o
            .next_report(&view, t0 + Duration::from_millis(8))
            .is_some());
        put(
            &mut o,
            &mut view,
            Key::AgentTitle(Slot(3)),
            title(3, "y"),
            t0,
        );
        put(&mut o, &mut view, Key::Reply(Slot(1)), reply(1, "r"), t0);
        let r = o
            .next_report(&view, t0 + Duration::from_millis(16))
            .unwrap();
        assert!(text(&r).contains("reply"), "title throttled");
        assert!(o
            .next_report(&view, t0 + Duration::from_millis(24))
            .is_none());
        let r = o
            .next_report(&view, t0 + Duration::from_millis(300))
            .unwrap();
        assert!(text(&r).contains("\"title\":\"y\""));
        // A title identical to the shadow is suppressed.
        put(
            &mut o,
            &mut view,
            Key::AgentTitle(Slot(3)),
            title(3, "y"),
            t0,
        );
        assert_eq!(o.depth(), 1);
        assert!(o.next_report(&view, t0 + Duration::from_secs(2)).is_none());
        assert_eq!(o.depth(), 0);
        o.new_sid(2);
        assert!(o.shadow().written.is_empty());
        assert_eq!(o.shadow().sid, 2);
        assert_eq!(o.sid(), 2);
    }

    #[test]
    fn aging_promotes_a_starved_usage_line() {
        let mut o = Outbox::new(Duration::from_millis(8), 1);
        let mut view = DeviceView::new();
        let t0 = Instant::now();
        put(
            &mut o,
            &mut view,
            Key::Usage,
            HostLine::Usage {
                period: "p".into(),
                items: vec![],
            },
            t0,
        );
        let later = t0 + Duration::from_millis(1100);
        put(&mut o, &mut view, Key::Reply(Slot(2)), reply(2, "r"), later);
        assert_eq!(o.depths(later).p2, 2, "usage aged from P3 into P2");
        let r = o.next_report(&view, later).unwrap();
        assert!(text(&r).contains("usage"), "P3 aged into P2, older wins");
        o.enqueue(&view, DeviceOp::Remove(Key::Reply(Slot(2))), later);
        assert_eq!(o.depth(), 0);
    }

    #[test]
    fn agent_state_is_full_then_a_diff_against_the_shadow() {
        let mut o = Outbox::new(Duration::from_millis(8), 1);
        let mut view = DeviceView::new();
        let t0 = Instant::now();
        put(
            &mut o,
            &mut view,
            Key::AgentState(Slot(2)),
            agent(2, AgentState::Working, Some(3)),
            t0,
        );
        assert_eq!(
            drain(&mut o, &view, t0),
            ["{\"m\":\"agent\",\"id\":2,\"kind\":\"claude\",\"name\":\"web\",\"state\":\"working\",\"since\":3}"]
        );
        put(
            &mut o,
            &mut view,
            Key::AgentState(Slot(2)),
            agent(2, AgentState::Ready, None),
            t0,
        );
        assert_eq!(
            drain(&mut o, &view, t0 + Duration::from_secs(1)),
            ["{\"m\":\"agent\",\"id\":2,\"state\":\"ready\"}"]
        );
        // Nothing changed: suppressed.
        put(
            &mut o,
            &mut view,
            Key::AgentState(Slot(2)),
            agent(2, AgentState::Ready, None),
            t0,
        );
        assert!(drain(&mut o, &view, t0 + Duration::from_secs(2)).is_empty());
        assert_eq!(o.depth(), 0);
        // Gone: one `gone` line, the shadow forgets the slot, the next agent is full again.
        let op = DeviceOp::Remove(Key::AgentState(Slot(2)));
        view.apply(&op);
        o.enqueue(&view, op, t0);
        assert_eq!(
            drain(&mut o, &view, t0 + Duration::from_secs(3)),
            ["{\"m\":\"agent\",\"id\":2,\"gone\":1}"]
        );
        assert!(o.shadow().written.is_empty());
        put(
            &mut o,
            &mut view,
            Key::AgentState(Slot(2)),
            agent(2, AgentState::Idle, None),
            t0,
        );
        let lines = drain(&mut o, &view, t0 + Duration::from_secs(4));
        assert!(
            lines[0].contains("\"kind\":\"claude\""),
            "full record after gone"
        );
        // A slot the device never had produces no `gone`.
        let op = DeviceOp::Remove(Key::AgentGone(Slot(7)));
        view.apply(&op);
        o.enqueue(&view, op, t0);
        assert_eq!(o.depth(), 0);
    }

    #[test]
    fn an_unsent_card_and_its_clear_cancel_and_focus_keeps_the_highest_ack() {
        let mut o = Outbox::new(Duration::from_millis(8), 1);
        let mut view = DeviceView::new();
        let t0 = Instant::now();
        put(&mut o, &mut view, Key::Card(Slot(1), 5), card(1, 5), t0);
        o.enqueue(&view, DeviceOp::Remove(Key::Card(Slot(1), 5)), t0);
        o.control(Control::CardClear(Slot(1), 5));
        assert_eq!(o.depth(), 0, "unsent card + clear = nothing");
        // A sent card is cleared for real, once.
        put(&mut o, &mut view, Key::Card(Slot(1), 6), card(1, 6), t0);
        assert_eq!(drain(&mut o, &view, t0).len(), 1);
        o.control(Control::CardClear(Slot(1), 6));
        o.control(Control::CardClear(Slot(1), 6));
        assert_eq!(o.depth(), 1);
        o.control(Control::Focus {
            slot: Slot(1),
            ack: Some(4),
        });
        o.control(Control::Focus {
            slot: Slot(2),
            ack: Some(9),
        });
        o.control(Control::Focus {
            slot: Slot(3),
            ack: Some(7),
        });
        o.control(Control::Hello);
        o.control(Control::Hello);
        o.set_clock(false, Some("19:42".into()));
        // Controls go out even while the lanes are held.
        put(&mut o, &mut view, Key::Reply(Slot(1)), reply(1, "held"), t0);
        o.hold_lanes(true);
        assert!(o.lanes_held());
        let lines = drain(&mut o, &view, t0 + Duration::from_secs(1));
        assert_eq!(
            lines,
            [
                "{\"m\":\"card\",\"agent\":1,\"id\":6,\"clear\":1}",
                "{\"m\":\"focus\",\"agent\":2,\"ack\":9}",
                "{\"m\":\"hello\",\"v\":1,\"sid\":1,\"hm\":\"19:42\"}",
            ]
        );
        assert_eq!(o.depth(), 1, "the reply waits");
        o.hold_lanes(false);
        assert_eq!(
            drain(&mut o, &view, t0 + Duration::from_secs(2)),
            ["{\"m\":\"reply\",\"agent\":1,\"text\":\"held\",\"rev\":1}"]
        );
    }

    #[test]
    fn a_put_during_the_barrier_is_dumped_once_and_written_keys_go_to_the_lanes() {
        let mut o = Outbox::new(Duration::from_millis(8), 1);
        let mut view = DeviceView::new();
        let t0 = Instant::now();
        view.apply(&DeviceOp::Put(
            Key::AgentState(Slot(1)),
            agent(1, AgentState::Idle, None),
        ));
        view.apply(&DeviceOp::Put(
            Key::AgentState(Slot(2)),
            agent(2, AgentState::Idle, None),
        ));
        view.apply(&DeviceOp::Put(Key::Reply(Slot(2)), reply(2, "old")));
        o.begin_barrier(&view, false);
        // Agent 1 goes out first (two reports).
        let r1 = o.next_report(&view, t0).unwrap();
        let r2 = o.next_report(&view, t0 + Duration::from_millis(8)).unwrap();
        let mut first = r1[3..3 + r1[2] as usize].to_vec();
        first.extend_from_slice(&r2[3..3 + r2[2] as usize]);
        assert_eq!(
            String::from_utf8(first).unwrap(),
            "{\"m\":\"agent\",\"id\":1,\"kind\":\"claude\",\"name\":\"web\",\"state\":\"idle\"}\n"
        );
        // Updates while the barrier runs: agent 1 (written) → lanes; reply 2 (unwritten) → view only.
        put(
            &mut o,
            &mut view,
            Key::AgentState(Slot(1)),
            agent(1, AgentState::Error, None),
            t0,
        );
        put(&mut o, &mut view, Key::Reply(Slot(2)), reply(2, "new"), t0);
        assert_eq!(o.depths(t0).p1, 1);
        assert_eq!(o.depths(t0).p2, 0);
        let lines = drain(&mut o, &view, t0 + Duration::from_millis(16));
        assert_eq!(
            lines,
            [
                "{\"m\":\"agent\",\"id\":2,\"kind\":\"claude\",\"name\":\"web\",\"state\":\"idle\"}",
                "{\"m\":\"reply\",\"agent\":2,\"text\":\"new\",\"rev\":1}",
                "{\"m\":\"synced\"}",
                "{\"m\":\"agent\",\"id\":1,\"state\":\"error\"}",
            ]
        );
        assert_eq!(o.shadow().written.len(), 3);
    }

    #[test]
    fn ws_and_macro_follow_the_face_gates() {
        let mut o = Outbox::new(Duration::from_millis(8), 1);
        let mut view = DeviceView::new();
        let t0 = Instant::now();
        view.set_faces(vec![
            FaceEntry {
                f: 2,
                t: Template::Omarchy,
                n: "O".into(),
            },
            FaceEntry {
                f: 3,
                t: Template::Macropad,
                n: "M".into(),
            },
        ]);
        let ws = HostLine::Ws {
            active: 1,
            count: 1,
            sel: 0,
            windows: vec![],
        };
        let mac = |f: u8| HostLine::Macro {
            face: f,
            src: None,
            keys: vec![],
        };
        put(&mut o, &mut view, Key::Ws, ws.clone(), t0);
        put(&mut o, &mut view, Key::Macro(FaceId(3)), mac(3), t0);
        put(&mut o, &mut view, Key::Macro(FaceId(5)), mac(5), t0);
        // Face unknown, strip has Omarchy: ws once per 2 s; macro 3 in the strip; macro 5 not.
        let lines = drain(&mut o, &view, t0);
        assert_eq!(
            lines,
            [
                "{\"m\":\"macro\",\"f\":3,\"keys\":[]}",
                "{\"m\":\"ws\",\"active\":1,\"count\":1,\"sel\":0,\"windows\":[]}"
            ]
        );
        assert_eq!(o.depth(), 1, "macro 5 waits for its face");
        // A ws update within 2 s waits; visible Omarchy face → at once.
        put(
            &mut o,
            &mut view,
            Key::Ws,
            HostLine::Ws {
                active: 1,
                count: 2,
                sel: 0,
                windows: vec![],
            },
            t0,
        );
        assert!(o
            .next_report(&view, t0 + Duration::from_millis(100))
            .is_none());
        view.set_visible_face(Some(2));
        o.reconsider(&view, t0 + Duration::from_millis(100));
        let r = o
            .next_report(&view, t0 + Duration::from_millis(108))
            .unwrap();
        assert!(text(&r).contains("\"count\":2"));
        // Agents face visible: ws is gated off.
        view.set_visible_face(Some(1));
        put(
            &mut o,
            &mut view,
            Key::Ws,
            HostLine::Ws {
                active: 1,
                count: 3,
                sel: 0,
                windows: vec![],
            },
            t0,
        );
        assert!(o.next_report(&view, t0 + Duration::from_secs(5)).is_none());
        assert_eq!(
            o.depths(t0 + Duration::from_secs(5)).p2,
            2,
            "ws and macro 5 aged, still gated"
        );
    }

    #[test]
    fn the_flood_fills_idle_ticks_and_yields_to_real_traffic() {
        let mut o = Outbox::new(Duration::from_millis(8), 1);
        let mut view = DeviceView::new();
        let t0 = Instant::now();
        // A 40-char reply is 74 bytes: two reports.
        put(
            &mut o,
            &mut view,
            Key::Reply(Slot(1)),
            reply(1, &"a".repeat(40)),
            t0,
        );
        let r = o.next_report(&view, t0).unwrap();
        assert_eq!((r[1], r[2]), (5, 61));
        let r = o.next_report(&view, t0 + Duration::from_millis(8)).unwrap();
        assert_eq!((r[1], r[2]), (5, 21), "the reply's second report");
        assert_eq!(o.depth(), 0);
        o.flood(3);
        assert_eq!(
            o.flood_state(),
            Flood {
                sent: 0,
                remaining: 3
            }
        );
        let lines = drain(&mut o, &view, t0 + Duration::from_millis(32));
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("Compiling openmicro ⠋ 0"));
        assert!(lines[2].contains("⠹ 2"));
        assert_eq!(
            o.flood_state(),
            Flood {
                sent: 3,
                remaining: 0
            }
        );
        // The flood yields to real traffic.
        o.flood(1);
        put(&mut o, &mut view, Key::Reply(Slot(1)), reply(1, "b"), t0);
        let lines = drain(&mut o, &view, t0 + Duration::from_secs(1));
        assert!(lines[0].contains("reply"));
        assert!(lines[1].contains("⠸ 3"));
        assert!(!o.burst(t0 + Duration::from_secs(1)));
        for _ in 0..BURST_LINES {
            o.recent_lines.push_back(t0 + Duration::from_secs(1));
        }
        assert!(o.burst(t0 + Duration::from_secs(2)));
        assert!(!o.burst(t0 + Duration::from_secs(4)), "window passed");
    }

    #[test]
    fn a_link_drop_requeues_the_line_in_flight_and_keeps_the_lanes() {
        let mut o = Outbox::new(Duration::from_millis(8), 1);
        let mut view = DeviceView::new();
        let t0 = Instant::now();
        put(
            &mut o,
            &mut view,
            Key::Reply(Slot(1)),
            reply(1, &"a".repeat(100)),
            t0,
        );
        assert!(o.next_report(&view, t0).is_some());
        o.report_written();
        put(
            &mut o,
            &mut view,
            Key::Set(SetKey::Mic),
            HostLine::Set {
                key: SetKey::Mic,
                value: 2,
            },
            t0,
        );
        let r = o.next_report(&view, t0 + Duration::from_millis(8)).unwrap();
        assert!(
            !text(&r).contains("set"),
            "the reply finishes first (atomicity)"
        );
        o.report_written();
        assert!(o.shadow().written.contains_key(&Key::Reply(Slot(1))));
        // The third report never reached the device: the reply is owed again.
        o.clear_link(t0 + Duration::from_millis(16));
        assert_eq!(o.depth(), 2, "set kept, the cut reply re-queued");
        assert!(
            !o.shadow().written.contains_key(&Key::Reply(Slot(1))),
            "the device never got the whole line"
        );
        let lines = drain(&mut o, &view, t0 + Duration::from_millis(16));
        assert_eq!(
            lines,
            [
                "{\"m\":\"set\",\"k\":\"mic\",\"v\":2}".to_string(),
                format!(
                    "{{\"m\":\"reply\",\"agent\":1,\"text\":\"{}\",\"rev\":1}}",
                    "a".repeat(100)
                ),
            ]
        );
        assert_eq!(o.encode_errors(), 0);
        // A control cut mid-way goes out again first; a line whose last
        // report was written is not re-sent.
        o.control(Control::Transcript {
            slot: Slot(1),
            text: "t".repeat(80),
            last: true, press: None, failed: false, channel_tip: false,
        });
        assert!(o.next_report(&view, t0 + Duration::from_secs(1)).is_some());
        o.report_written();
        o.clear_link(t0 + Duration::from_secs(1));
        put(&mut o, &mut view, Key::Reply(Slot(2)), reply(2, "r"), t0);
        let lines = drain(&mut o, &view, t0 + Duration::from_secs(1));
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("{\"m\":\"transcript\""));
        assert!(o.next_report(&view, t0 + Duration::from_secs(2)).is_none());
        o.clear_link(t0 + Duration::from_secs(2));
        assert_eq!(o.depth(), 0, "nothing in flight after a whole line");
        put(
            &mut o,
            &mut view,
            Key::AgentTitle(Slot(0)),
            title(0, "bad slot"),
            t0,
        );
        assert!(drain(&mut o, &view, t0 + Duration::from_secs(1)).is_empty());
        assert_eq!(
            o.encode_errors(),
            1,
            "an unencodable line is dropped, not sent"
        );
        assert_eq!(o.recent(1)[0].0, "reply");
    }

    /// One core apply reassigns a slot: `gone` for the old agent, the new
    /// agent, its card — all at one instant. The wire must say gone first,
    /// then the full record (never a diff of the predecessor), then the card.
    #[test]
    fn a_slot_reassigned_in_one_batch_sends_gone_then_the_full_agent_then_its_card() {
        let mut o = Outbox::new(Duration::from_millis(8), 1);
        let mut view = DeviceView::new();
        let t0 = Instant::now();
        let old = HostLine::Agent {
            slot: 2,
            kind: Some(Kind::Claude),
            name: Some("old".into()),
            project: None,
            state: Some(AgentState::Working),
            title: None,
            since_s: None,
        };
        put(&mut o, &mut view, Key::AgentState(Slot(2)), old, t0);
        assert_eq!(drain(&mut o, &view, t0).len(), 1);
        let new = HostLine::Agent {
            slot: 2,
            kind: Some(Kind::Claude),
            name: Some("new".into()),
            project: None,
            state: Some(AgentState::Question),
            title: None,
            since_s: None,
        };
        let t1 = t0 + Duration::from_secs(1);
        for op in [
            DeviceOp::Put(Key::AgentGone(Slot(2)), HostLine::AgentGone { slot: 2 }),
            DeviceOp::Remove(Key::AgentState(Slot(2))),
            DeviceOp::Put(Key::AgentState(Slot(2)), new),
            DeviceOp::Put(Key::Card(Slot(2), 1), card(2, 1)),
        ] {
            view.apply(&op);
            o.enqueue(&view, op, t1);
        }
        assert_eq!(
            drain(&mut o, &view, t1),
            [
                "{\"m\":\"agent\",\"id\":2,\"gone\":1}",
                "{\"m\":\"agent\",\"id\":2,\"kind\":\"claude\",\"name\":\"new\",\"state\":\"question\"}",
                "{\"m\":\"card\",\"agent\":2,\"id\":1,\"type\":\"q\",\"text\":\"t\"}",
            ]
        );
        assert_eq!(o.shadow().written.len(), 2, "the new agent and its card");
        // The same batch landing on a same-sid barrier: gone still leads.
        let t2 = t1 + Duration::from_secs(1);
        let op = DeviceOp::Remove(Key::AgentState(Slot(2)));
        view.apply(&op);
        o.enqueue(&view, op, t2);
        put(
            &mut o,
            &mut view,
            Key::AgentState(Slot(2)),
            agent(2, AgentState::Idle, None),
            t2,
        );
        o.begin_barrier(&view, false);
        let lines = drain(&mut o, &view, t2);
        assert_eq!(lines[0], "{\"m\":\"agent\",\"id\":2,\"gone\":1}");
        assert!(lines[1].contains("\"kind\":\"claude\""));
        assert_eq!(lines.last().unwrap(), "{\"m\":\"synced\"}");
    }

    #[test]
    fn conol_creation_focus_follows_slot_replacement_even_when_reusing_the_focused_slot() {
        for already_focused in [false, true] {
            let t0 = Instant::now();
            let mut o = Outbox::new(PACE_FLOOR, 1);
            let mut view = DeviceView::new();
            let old = HostLine::Agent {
                slot: 2, kind: Some(Kind::Conol), name: Some("old chat".into()),
                project: Some("Conol".into()), state: Some(AgentState::Working), title: None, since_s: None,
            };
            put(&mut o, &mut view, Key::AgentState(Slot(2)), old, t0);
            drain(&mut o, &view, t0);
            if already_focused {
                put(&mut o, &mut view, Key::Focus, HostLine::Focus { slot: 2, ack: None }, t0);
                drain(&mut o, &view, t0 + Duration::from_secs(1));
            }
            let t1 = t0 + Duration::from_secs(2);
            let new = HostLine::Agent {
                slot: 2, kind: Some(Kind::Conol), name: Some("new chat".into()),
                project: Some("Conol".into()), state: Some(AgentState::Working), title: None, since_s: None,
            };
            for op in [
                DeviceOp::Put(Key::AgentGone(Slot(2)), HostLine::AgentGone { slot: 2 }),
                DeviceOp::Remove(Key::AgentState(Slot(2))),
                DeviceOp::Put(Key::AgentState(Slot(2)), new),
                DeviceOp::Put(Key::Focus, HostLine::Focus { slot: 2, ack: None }),
            ] {
                view.apply(&op);
                o.enqueue(&view, op, t1);
            }
            let lines = drain(&mut o, &view, t1);
            assert_eq!(lines.len(), 3, "focus must be sent for the new occupant: {lines:?}");
            assert!(lines[0].contains("\"gone\":1"), "old occupant must leave first: {lines:?}");
            assert!(lines[1].contains("new chat"), "the new chat must exist before focus: {lines:?}");
            assert_eq!(lines[2], "{\"m\":\"focus\",\"agent\":2}");
        }
    }

    #[test]
    fn a_new_slot_is_announced_before_its_focus() {
        let t0 = Instant::now();
        let mut o = Outbox::new(PACE_FLOOR, 1);
        let mut view = DeviceView::new();
        put(&mut o, &mut view, Key::AgentState(Slot(2)), agent(2, AgentState::Working, None), t0);
        put(&mut o, &mut view, Key::Focus, HostLine::Focus { slot: 2, ack: None }, t0);
        let lines = drain(&mut o, &view, t0);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"kind\""), "the device ignores focus for unknown slots: {lines:?}");
        assert_eq!(lines[1], "{\"m\":\"focus\",\"agent\":2}");
    }

    /// A device `channel` withdraws the PC focus (`Remove(Focus)` after the
    /// ack): the next PC focus on the same agent is a real change again.
    #[test]
    fn a_removed_key_leaves_the_shadow_so_an_identical_put_is_sent_again() {
        let mut o = Outbox::new(Duration::from_millis(8), 1);
        let mut view = DeviceView::new();
        let t0 = Instant::now();
        let focus1 = HostLine::Focus { slot: 1, ack: None };
        put(&mut o, &mut view, Key::Focus, focus1.clone(), t0);
        assert_eq!(drain(&mut o, &view, t0), ["{\"m\":\"focus\",\"agent\":1}"]);
        o.control(Control::Focus {
            slot: Slot(3),
            ack: Some(7),
        });
        let op = DeviceOp::Remove(Key::Focus);
        view.apply(&op);
        o.enqueue(&view, op, t0);
        assert_eq!(
            drain(&mut o, &view, t0),
            ["{\"m\":\"focus\",\"agent\":3,\"ack\":7}"]
        );
        put(&mut o, &mut view, Key::Focus, focus1, t0);
        assert_eq!(
            drain(&mut o, &view, t0 + Duration::from_secs(1)),
            ["{\"m\":\"focus\",\"agent\":1}"],
            "the device moved the bar: the PC's focus applies again"
        );
        // Any view key: a reply removed and re-put identically is written again.
        put(&mut o, &mut view, Key::Reply(Slot(1)), reply(1, "r"), t0);
        assert_eq!(drain(&mut o, &view, t0 + Duration::from_secs(2)).len(), 1);
        let op = DeviceOp::Remove(Key::Reply(Slot(1)));
        view.apply(&op);
        o.enqueue(&view, op, t0);
        put(&mut o, &mut view, Key::Reply(Slot(1)), reply(1, "r"), t0);
        assert_eq!(
            drain(&mut o, &view, t0 + Duration::from_secs(3)),
            ["{\"m\":\"reply\",\"agent\":1,\"text\":\"r\",\"rev\":1}"]
        );
    }

    /// A cleared card leaves the shadow with its clear: no growth per card
    /// ever shown, and a clear for a long-gone id is not written.
    #[test]
    fn the_shadow_forgets_a_card_when_its_clear_is_written() {
        let mut o = Outbox::new(Duration::from_millis(8), 1);
        let mut view = DeviceView::new();
        let mut t = Instant::now();
        let baseline = o.shadow().written.len();
        for i in 1..=50u16 {
            put(&mut o, &mut view, Key::Card(Slot(1), i), card(1, i), t);
            assert_eq!(drain(&mut o, &view, t).len(), 1);
            t += Duration::from_secs(1);
            o.control(Control::CardClear(Slot(1), i));
            let op = DeviceOp::Remove(Key::Card(Slot(1), i));
            view.apply(&op);
            o.enqueue(&view, op, t);
            assert_eq!(
                drain(&mut o, &view, t),
                [format!(
                    "{{\"m\":\"card\",\"agent\":1,\"id\":{i},\"clear\":1}}"
                )]
            );
            t += Duration::from_secs(1);
            assert_eq!(o.shadow().written.len(), baseline, "flat after card {i}");
        }
        assert_eq!(view.len(), 0);
        o.control(Control::CardClear(Slot(1), 50));
        assert_eq!(
            o.depth(),
            0,
            "a clear for a cleared card is not written twice"
        );
        // The replace path (`Remove` without a clear) does not leak either.
        put(&mut o, &mut view, Key::Card(Slot(1), 51), card(1, 51), t);
        drain(&mut o, &view, t);
        let op = DeviceOp::Remove(Key::Card(Slot(1), 51));
        view.apply(&op);
        o.enqueue(&view, op, t);
        assert_eq!(o.shadow().written.len(), baseline);
    }
}
