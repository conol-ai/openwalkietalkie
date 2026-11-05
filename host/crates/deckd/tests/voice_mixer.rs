#![cfg(target_os = "macos")]
//! §15's week-4 rows 4 and 5, and the one claim §4 makes that no ordinary
//! test can check: **the render callback allocates nothing.**
//!
//! Everything here is driven by a fake render callback at **512-frame
//! quanta**, which is not an arbitrary number: V18 read
//! `thread_policy_get(THREAD_TIME_CONSTRAINT_POLICY)` from inside a live
//! callback on the deck's own output and got 512 frames / 11.610 ms, with
//! `period == computation == constraint` and inter-callback times of
//! 11.44–11.65 ms. So one quantum here is one quantum there, a 10 ms fade
//! spans one of them, and §15's "−60 dB within 35 ms" is three.
//!
//! The allocation row uses a counting allocator rather than a reviewer's
//! promise, because §7's rule — "a PR that adds an allocation or a lock to
//! the callback is wrong even if every test passes" — is exactly the kind of
//! rule that decays into a comment. The counter is **thread-local**: the
//! test harness allocates on its own threads while this one runs, and a
//! global count would be measuring cargo.
use deckd::audio::lanes::{Lane, Lanes, Mixer, DUCK_GAIN, LANE_COUNT};
use deckd::audio::traits::{Fault, StreamFaults};
use deckd::audio::LANE_RATE;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::time::{Duration, Instant};

thread_local! {
    /// Allocations made by *this* thread. `Cell<usize>` has no destructor,
    /// so the thread-local needs no lazy allocation of its own — which
    /// would otherwise be an allocation inside the allocator.
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: every call is forwarded to the system allocator unchanged; the
// only addition is a thread-local counter bump, and `try_with` keeps it from
// touching TLS that is being torn down.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        unsafe { System.alloc_zeroed(layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn allocs() -> usize {
    ALLOCS.with(Cell::get)
}

/// V18's quantum.
const QUANTUM: usize = 512;

struct Rig {
    lanes: Arc<Lanes>,
    producers: [Option<rtrb::Producer<f32>>; LANE_COUNT],
    out: Vec<f32>,
}

impl Rig {
    fn new() -> Rig {
        let lanes = Lanes::new();
        let producers = [
            lanes.take_producer(Lane::Chime),
            lanes.take_producer(Lane::Question),
            lanes.take_producer(Lane::Ready),
            lanes.take_producer(Lane::Chatter),
        ];
        Rig {
            lanes,
            producers,
            out: vec![0.0; QUANTUM * 2],
        }
    }

    /// `frames` of a constant level, so the mixer's output level *is* the
    /// gain it applied and nothing has to be reasoned about in the
    /// frequency domain.
    fn fill(&mut self, lane: Lane, frames: usize, level: f32) {
        let producer = self.producers[lane.index()].as_mut().expect("producer");
        for _ in 0..frames {
            producer.push(level).expect("the ring is 2 s deep");
            producer.push(level).expect("the ring is 2 s deep");
        }
        self.lanes.set_active(lane, true);
    }

    /// One 512-frame callback; returns the left channel.
    fn quantum(&mut self, mixer: &mut Mixer) -> Vec<f32> {
        self.out.iter_mut().for_each(|s| *s = 0.0);
        mixer.render(&mut self.out);
        self.out.iter().step_by(2).copied().collect()
    }

    /// `n` quanta, concatenated.
    fn quanta(&mut self, mixer: &mut Mixer, n: usize) -> Vec<f32> {
        let mut all = Vec::new();
        for _ in 0..n {
            all.extend(self.quantum(mixer));
        }
        all
    }
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

fn ms(frames: usize) -> f64 {
    frames as f64 * 1000.0 / f64::from(LANE_RATE)
}

/// §15 week-4 row 4.
///
/// Every clause is one of §7.3's rules, measured on the quantum V18 found
/// in the real callback.
#[test]
fn a_higher_lane_cuts_and_ducks_within_its_budget() {
    // ---- the PTT cut reaches −60 dB within 35 ms -------------------------
    let mut rig = Rig::new();
    let mut mixer = rig.lanes.take_mixer().expect("the first mixer");
    rig.fill(Lane::Question, LANE_RATE as usize, 1.0); // a second of it
    let before = rig.quantum(&mut mixer);
    assert!(
        (peak(&before) - 1.0).abs() < 1e-6,
        "the question plays at unity first: {}",
        peak(&before)
    );

    rig.lanes.set_ptt(true);
    // Three quanta is 34.8 ms — deliberately just inside the budget, so a
    // fade that took four would fail here rather than in someone's ear.
    let cutting = rig.quanta(&mut mixer, 3);
    let silent_from = cutting
        .iter()
        .enumerate()
        .rev()
        .take_while(|(_, s)| s.abs() <= 0.001)
        .map(|(i, _)| i)
        .last()
        .expect("the cut has to reach silence at all");
    assert!(
        ms(silent_from) <= 35.0,
        "§15: −60 dB within 35 ms; reached at {:.2} ms",
        ms(silent_from)
    );
    assert!(
        ms(silent_from) >= 9.0,
        "and it is a *fade*, not a click: {:.2} ms",
        ms(silent_from)
    );

    // ---- Question and Ready survive the hold (§7.3: held, not flushed) ---
    rig.fill(Lane::Ready, LANE_RATE as usize / 2, 0.25);
    let queued_during = (mixer.queued(Lane::Question), mixer.queued(Lane::Ready));
    assert!(queued_during.0 > 0 && queued_during.1 > 0);
    let _ = rig.quanta(&mut mixer, 10);
    assert_eq!(
        (mixer.queued(Lane::Question), mixer.queued(Lane::Ready)),
        queued_during,
        "a PTT hold consumes nothing: the operator has not heard it yet"
    );

    // ---- and they queue FIFO by priority when the hold ends --------------
    rig.lanes.set_ptt(false);
    let resumed = rig.quanta(&mut mixer, 2);
    assert!(
        (peak(&resumed) - 1.0).abs() < 1e-3,
        "the question resumes, not the ready: {}",
        peak(&resumed)
    );
    // Drain the question and watch the floor pass to Ready — not before.
    rig.lanes.set_active(Lane::Question, false);
    while mixer.queued(Lane::Question) > 0 {
        let _ = rig.quantum(&mut mixer);
    }
    let after = rig.quanta(&mut mixer, 3);
    assert!(
        (peak(&after) - 0.25).abs() < 1e-2,
        "then Ready, at its own level: {}",
        peak(&after)
    );

    // ---- Chime ducks the lower lanes to −18 dB ---------------------------
    // The chime itself is *busy but silent*, which is exactly the state a
    // chime is in between "queued" and "its first sample": the duck starts
    // then, or it starts too late to be a duck.
    rig.lanes.set_active(Lane::Chime, true);
    let ducked = rig.quanta(&mut mixer, 20); // > 120 ms of ramp
    let settled = peak(&ducked[ducked.len() - QUANTUM..]);
    assert!(
        (settled - 0.25 * DUCK_GAIN).abs() < 1e-3,
        "−18 dB under the chime: expected {}, got {settled}",
        0.25 * DUCK_GAIN
    );
    rig.lanes.set_active(Lane::Chime, false);

    // ---- a Question cuts Chatter ----------------------------------------
    let mut rig = Rig::new();
    drop(mixer);
    let mut mixer = rig.lanes.take_mixer().expect("a fresh mixer");
    rig.fill(Lane::Chatter, LANE_RATE as usize, 0.5);
    assert!((peak(&rig.quantum(&mut mixer)) - 0.5).abs() < 1e-6);
    rig.fill(Lane::Question, LANE_RATE as usize, 0.75);
    let _ = rig.quanta(&mut mixer, 4);
    assert_eq!(
        mixer.queued(Lane::Chatter),
        0,
        "§7.3: Question/Ready *cut* Chatter — it is thrown away, not held"
    );
    let only_question = rig.quantum(&mut mixer);
    assert!(
        (peak(&only_question) - 0.75).abs() < 1e-3,
        "and what is left is the question alone: {}",
        peak(&only_question)
    );

    // ---- the Chatter TTL drops ------------------------------------------
    let mut rig = Rig::new();
    drop(mixer);
    let mut mixer = rig.lanes.take_mixer().expect("a third mixer");
    let now = Instant::now();
    rig.fill(Lane::Chatter, LANE_RATE as usize, 0.5);
    rig.lanes.arm_ttl(Lane::Chatter, now);
    assert!(
        rig.lanes.expire(now + Duration::from_secs(19)).is_empty(),
        "20 s is 20 s"
    );
    assert_eq!(
        rig.lanes.expire(now + Duration::from_secs(21)),
        vec![Lane::Chatter]
    );
    rig.lanes.set_active(Lane::Chatter, false);
    let _ = rig.quanta(&mut mixer, 4);
    assert_eq!(
        mixer.queued(Lane::Chatter),
        0,
        "an expired lane is discarded by the callback, not by a lock"
    );
}

/// §7.3, at the one moment a render closure has no history: its first
/// quantum.
///
/// §7.2 tears the output down and rebuilds it on every press and every rate
/// flip, so "the first quantum of a fresh mixer" is not an edge case — it
/// is what the operator hears after every single PTT release. A mixer whose
/// cut envelopes start at unity fades every **held** lane down from full
/// level and drains it while doing so, which is both audible and a
/// contradiction of §7.3's "`Question`/`Ready` are held, not flushed".
/// Measured before the fix: 1.0494 peak — over full scale — on the first
/// quantum with a Question and a Ready queued, and 880 frames of the Ready
/// consumed.
#[test]
fn a_fresh_mixer_fades_in_and_holds_everything_it_is_not_playing() {
    let mut rig = Rig::new();
    rig.fill(Lane::Question, LANE_RATE as usize, 0.8);
    rig.fill(Lane::Ready, LANE_RATE as usize, 0.25);

    let mut mixer = rig.lanes.take_mixer().expect("a brand-new stream");
    let first = rig.quantum(&mut mixer);

    assert!(
        peak(&first) <= 1.0,
        "a fresh closure may not clip: peak {}",
        peak(&first)
    );
    assert!(
        first[0].abs() < 0.01,
        "it fades in from silence rather than opening at unity: {}",
        first[0]
    );
    assert_eq!(
        rig.lanes.played(Lane::Ready),
        0,
        "§7.3: the Ready underneath is held, not drained under a fade-out \
         of a signal that was never playing"
    );
    // …and the fade is the same 10 ms as every other one, so the lane that
    // *is* playing is at full level well inside one 11.61 ms quantum.
    assert!(
        (peak(&first) - 0.8).abs() < 1e-3,
        "the Question reaches its own level within the quantum: {}",
        peak(&first)
    );
}

/// V3's other half, on the path that actually uses it.
///
/// `Reopening` opens the output at the device's **current** nominal rate —
/// 16000 through the SCO tail, because waiting for 44100 costs 2591 ms
/// against 48 ms — and the lanes are always 44 100
/// (`AudioFormat::lane()`), `Resample` always targets the lane rate and
/// `ChimeBank` only renders at it. Nothing between the two resampled, so
/// `render` popped lane samples 1:1 into a 16 kHz stream: **every**
/// post-PTT announcement — §7.7's ready reply, the "not delivered" line,
/// the question chime — played at 0.363× speed and about sixteen semitones
/// flat until A2DP came back 2.3 s later, and the whole of `ScoStuck` did.
/// route.rs's own doc said the machine "opens immediately **and resamples
/// into the live rate**"; that half was never written.
///
/// The fix must not be "pass `LANE_RATE` to cpal": that makes cpal call
/// `set_physical_format`/`set_sample_rate` and fight the HFP link.
#[test]
fn the_release_plays_the_announcement_at_pitch_into_the_sco_tail() {
    use deck_model::{AudioCmd, PttCtx, Route, VoiceInput};
    use deckd::audio::engine::{Engine, NullOutput};
    use deckd::audio::traits::{DeviceSide, RouteEvent};
    use deckd::audio::FromAudio;

    /// A tone the SCO rate can carry, and low enough that neither rate is
    /// anywhere near folding it.
    const TONE: f32 = 1_000.0;
    const SCO: u32 = 16_000;

    let lanes = Lanes::new();
    let (to_core, _from_core) = crossbeam_channel::unbounded::<FromAudio>();
    let (capture, _capture_handle) = deckd::audio::fakes::FakeCapture::new();
    let mut engine = Engine::new(
        lanes.clone(),
        Box::new(capture),
        Box::new(NullOutput::default()),
        to_core,
    );
    let t0 = Instant::now();
    engine.on_device(DeviceSide::Output, true, t0);
    engine.on_route(RouteEvent::NominalRate { rate: LANE_RATE }, t0);

    // A press, the rate following us down to SCO, and the release.
    engine.on_cmd(
        AudioCmd::Listen {
            agent: None,
            ctx: PttCtx::Talk,
            card: None,
            max_s: 60,
            from_deck: true,
            input: VoiceInput::Deck,
            locale: "en-US".into(),
            contextual: Vec::new(),
            companion: None,
        },
        t0,
    );
    engine.on_route(RouteEvent::NominalRate { rate: SCO }, t0);
    engine.on_cmd(AudioCmd::Finish { abort: true }, t0);
    assert_eq!(engine.route(), Route::Reopening);

    // §7.7's reply, synthesised into the lane at the lane rate, as every
    // producer of lane audio does.
    let mut producer = lanes.take_producer(Lane::Ready).expect("producer");
    for i in 0..LANE_RATE as usize {
        #[allow(clippy::cast_precision_loss)]
        let t = i as f32 / LANE_RATE as f32;
        let sample = 0.5 * (std::f32::consts::TAU * TONE * t).sin();
        producer.push(sample).expect("2 s of ring");
        producer.push(sample).expect("2 s of ring");
    }
    lanes.set_active(Lane::Ready, true);

    // One second of output at the rate the device is actually running.
    let quanta = SCO as usize / QUANTUM;
    let mut out = vec![0.0f32; QUANTUM * 2];
    let mut left: Vec<f32> = Vec::new();
    for _ in 0..quanta {
        out.iter_mut().for_each(|s| *s = 0.0);
        assert!(
            engine.output_mut().render_now(&mut out),
            "V3: open at once, at the tail rate"
        );
        left.extend(out.iter().step_by(2).copied());
    }

    // **Pitch.** A 1 kHz tone crosses zero 2000 times a second whatever
    // rate it is clocked out at; at 1:1 into a 16 kHz stream it comes out
    // at 1000 × 16000/44100 = 363 Hz, which is what the deck was playing.
    // The first quantum is skipped: the cut envelope starts there.
    let tail = &left[QUANTUM..];
    let crossings = tail
        .windows(2)
        .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
        .count();
    #[allow(clippy::cast_precision_loss)]
    let hz = crossings as f32 / 2.0 * SCO as f32 / tail.len() as f32;
    assert!(
        (hz - TONE).abs() < 20.0,
        "the announcement plays at {hz:.0} Hz, not {TONE:.0} Hz \
         (1:1 into the SCO tail is 363 Hz — sixteen semitones flat)"
    );

    // **Duration.** One second of output must consume one second of lane.
    #[allow(clippy::cast_precision_loss)]
    let want = (quanta * QUANTUM) as f64 * f64::from(LANE_RATE) / f64::from(SCO);
    #[allow(clippy::cast_precision_loss)]
    let consumed = lanes.played(Lane::Ready) as f64;
    assert!(
        (consumed - want).abs() < want * 0.005,
        "{consumed} lane frames for {} output frames; {want:.0} expected",
        quanta * QUANTUM
    );
}

/// §7.2/§7.3, and the one thing a `flush` must never do: reach the **next**
/// item on the lane.
///
/// The flag was only ever cleared by a render callback, and there is not
/// always one. `Store::start_listen`'s barge-in and `Engine::listen`'s close
/// land in the same core turn; `voice.stop` arrives while the route is
/// `ScoOurs` and the output is shut; a `Chime` TTL expires during a hold.
/// Measured on the real shape: with no render quantum between the `Stop` and
/// the close, 44 100 queued samples of a held Question were discarded and
/// the operator heard a 10 ms blip — with one quantum in between, the same
/// audio survived. One press, two outcomes, decided by a race with the
/// 11.61 ms callback period. Neither outcome may depend on that race, and
/// the audio a flush discards must be the audio that was there when it was
/// raised.
#[test]
fn a_flush_while_nothing_is_rendering_cannot_eat_the_next_item() {
    // (a) No stream at all — `voice.stop` during `ScoOurs`, or a TTL
    //     expiring mid-hold. The ring is emptied when the flush is raised,
    //     and what is queued afterwards is somebody else's audio.
    let mut rig = Rig::new();
    rig.fill(Lane::Question, 1_000, 0.5);
    assert!(rig.lanes.queued(Lane::Question) > 0);
    rig.lanes.flush(Lane::Question);
    assert_eq!(
        rig.lanes.queued(Lane::Question),
        0,
        "a flush with nothing rendering is honoured where it is raised"
    );

    rig.fill(Lane::Question, 1_000, 0.5); // the *next* Question
    let mut mixer = rig.lanes.take_mixer().expect("the next stream");
    let played = rig.quanta(&mut mixer, 2);
    assert!(
        (peak(&played) - 0.5).abs() < 1e-3,
        "the next item plays; the old flag did not follow it: {}",
        peak(&played)
    );
    drop(mixer);

    // (b) The measured race: the flag is raised while a closure holds the
    //     consumer, and the stream is torn down before its next callback.
    let mut rig = Rig::new();
    let mixer = rig.lanes.take_mixer().expect("the first stream");
    rig.fill(Lane::Question, 1_000, 0.5);
    rig.lanes.flush(Lane::Question); // `voice.stop`, between two quanta
    drop(mixer); // `AudioCmd::Listen` closes the output
    assert_eq!(
        rig.lanes.queued(Lane::Question),
        0,
        "the closure that owed the flush honoured it on the way out"
    );

    rig.fill(Lane::Question, 1_000, 0.5); // queued during the hold
    let mut mixer = rig.lanes.take_mixer().expect("the release rebuilds it");
    let resumed = rig.quanta(&mut mixer, 2);
    assert!(
        (peak(&resumed) - 0.5).abs() < 1e-3,
        "§7.3: the Question queued during the hold is held, not eaten by a \
         flush raised before it existed: {}",
        peak(&resumed)
    );
}

/// §15 week-4 row 5.
///
/// §7.2's whole reason for the `Mutex<Option<Consumer>>` dance: a PTT press
/// tears the output stream down, and the Question the operator has not heard
/// yet must still be there when the next one is built. V10 corrected *why*
/// (the closure is `Send + 'static` and dies with the stream, not that
/// `Stream` is `!Send`) — it did not change *that*.
#[test]
fn the_lanes_survive_a_stream_rebuild() {
    let mut rig = Rig::new();
    let mut mixer = rig.lanes.take_mixer().expect("the first stream's closure");
    rig.fill(Lane::Question, LANE_RATE as usize / 2, 0.8);
    let played = rig.quanta(&mut mixer, 2);
    assert!((peak(&played) - 0.8).abs() < 1e-6);
    let left_after_first = mixer.queued(Lane::Question);
    assert!(
        left_after_first > 0,
        "half a second is more than two quanta"
    );

    // The press: the stream is dropped, and with it the render closure.
    drop(mixer);
    assert_eq!(
        rig.lanes.queued(Lane::Question),
        left_after_first,
        "dropping the closure must not drop the ring"
    );

    // The release: a new stream, a new closure, the same rings.
    let mut mixer = rig.lanes.take_mixer().expect("the second stream's closure");
    let resumed = rig.quanta(&mut mixer, 2);
    assert!(
        (peak(&resumed) - 0.8).abs() < 1e-6,
        "and it plays on: {}",
        peak(&resumed)
    );
    assert!(mixer.queued(Lane::Question) < left_after_first);
    assert!(rig.lanes.played(Lane::Question) >= 4 * QUANTUM as u64);
}

/// §4, and V19 for the second half.
///
/// The render callback and the error callback both run on the CoreAudio
/// real-time thread — V18 read `period = computation = constraint =
/// 11.610 ms` from inside one — so neither may allocate. This counts.
#[test]
fn the_render_and_error_callbacks_allocate_nothing() {
    let mut rig = Rig::new();
    let mut mixer = rig.lanes.take_mixer().expect("mixer");
    rig.fill(Lane::Question, LANE_RATE as usize, 0.5);
    rig.fill(Lane::Chatter, LANE_RATE as usize, 0.5);
    rig.lanes.set_active(Lane::Chime, true);
    let faults = StreamFaults::new();

    // Warm every lazily-initialised thing the first call might touch.
    rig.out.iter_mut().for_each(|s| *s = 0.0);
    mixer.render(&mut rig.out);
    faults.raise(Fault::Xrun);

    // 100 quanta is 1.16 s of audio, through the cut, the duck and the
    // discard paths, plus an error callback per quantum.
    let before = allocs();
    for i in 0..100 {
        rig.lanes.set_ptt(i % 32 == 0);
        if i % 16 == 0 {
            rig.lanes.flush(Lane::Chatter);
        }
        rig.out.iter_mut().for_each(|s| *s = 0.0);
        mixer.render(&mut rig.out);
        faults.raise(if i % 3 == 0 {
            Fault::Xrun
        } else {
            Fault::Invalidated
        });
    }
    let after = allocs();
    assert_eq!(
        after,
        before,
        "the render and error callbacks allocated {} times in 100 quanta",
        after - before
    );
    assert!(faults.xruns() >= 34, "and the counter still counted");
}

/// §15's week-4 row 1, **the mixer half**: what a press does to what is
/// playing, and what a release gives back.
///
/// Driven through the real control loop rather than the lanes alone,
/// because the interesting part is the interaction: `Listen` cuts the
/// mixer *and* closes the output (§7.2/V5 — the deck returns our own TTS to
/// its own mic at ≈ +17 dB with no AEC), so the queued Question has to
/// survive both the cut and the stream teardown, and come back at the rate
/// the device has at the release (V3).
#[test]
fn a_press_cuts_the_mixer_and_the_release_gives_the_queued_audio_back() {
    use deck_model::{AudioCmd, PttCtx, Route, VoiceIn, VoiceInput};
    use deckd::audio::engine::{Engine, NullOutput};
    use deckd::audio::traits::{DeviceSide, RouteEvent};
    use deckd::audio::FromAudio;

    let lanes = Lanes::new();
    let mut producer = lanes.take_producer(Lane::Question).expect("producer");
    // Half a second of a Question, already queued when the operator presses.
    let pushed = LANE_RATE as usize; // half a second, stereo
    for _ in 0..(LANE_RATE as usize / 2) {
        producer.push(0.8).unwrap();
        producer.push(0.8).unwrap();
    }
    lanes.set_active(Lane::Question, true);

    let (to_core, from_core) = crossbeam_channel::unbounded::<FromAudio>();
    let (capture, capture_handle) = deckd::audio::fakes::FakeCapture::new();
    let mut engine = Engine::new(
        lanes.clone(),
        Box::new(capture),
        Box::new(NullOutput::default()),
        to_core,
    );
    let t0 = Instant::now();
    engine.on_device(DeviceSide::Output, true, t0);
    engine.on_route(RouteEvent::NominalRate { rate: 44_100 }, t0);
    assert_eq!(engine.route(), Route::A2dp);

    // It is playing.
    let mut out = vec![0.0f32; QUANTUM * 2];
    let render = |engine: &mut Engine, out: &mut Vec<f32>| -> Option<f32> {
        out.iter_mut().for_each(|s| *s = 0.0);
        engine
            .output_mut()
            .render_now(out)
            .then(|| peak(&out.iter().step_by(2).copied().collect::<Vec<_>>()))
    };
    assert!((render(&mut engine, &mut out).expect("playing") - 0.8).abs() < 1e-6);
    // `Lanes::queued` answers 0 while a stream holds the consumer, so the
    // count that matters is taken after the press — and it is exact: one
    // 512-frame quantum came out of what was pushed.
    let left_after_one_quantum = pushed - QUANTUM * 2;

    // `ptt act:1`, both commands, in the order `Store::start_listen`
    // pushes them. The barge-in used to be `Stop{all:true}` and used to
    // take this very Question with it — §7.3 holds `Question`/`Ready` and
    // flushes stale `Chatter`, and the press side now says so too
    // (`deck_model::voice`'s `a_press_cuts_audio_opens_capture_…` pins the
    // exact command).
    engine.on_cmd(
        AudioCmd::Stop {
            utterance: None,
            lane: Some("chatter".into()),
            all: false,
        },
        t0,
    );
    engine.on_cmd(
        AudioCmd::Listen {
            agent: None,
            ctx: PttCtx::Answer,
            card: None,
            max_s: 60,
            from_deck: true,
            input: VoiceInput::Deck,
            locale: "en-US".into(),
            contextual: Vec::new(),
            companion: None,
        },
        t0,
    );
    assert!(lanes.ptt(), "§7.3's virtual Ptt lane: everything is cut");
    assert_eq!(
        capture_handle.opens().len(),
        1,
        "§7.5.1: the mic went first"
    );
    assert!(
        render(&mut engine, &mut out).is_none(),
        "§7.2/V5: the output is closed while we capture"
    );
    assert_eq!(
        lanes.queued(Lane::Question),
        left_after_one_quantum,
        "and the Question the operator has not heard is still there, to the \
         sample: the stream was torn down and took nothing with it"
    );

    // Some chatter arrived while the button was held. §7.2 flushes it at
    // the release: it is a reply to something the operator has since
    // talked over, and unlike a Question nobody is waiting on it.
    let mut chatter = lanes.take_producer(Lane::Chatter).expect("producer");
    for _ in 0..1000 {
        chatter.push(0.3).unwrap();
        chatter.push(0.3).unwrap();
    }
    assert!(lanes.queued(Lane::Chatter) > 0);

    // The rate follows us down, then the release. V3: the reopen is
    // immediate, at 16000.
    engine.on_route(RouteEvent::NominalRate { rate: 16_000 }, t0);
    assert_eq!(engine.route(), Route::ScoOurs);
    engine.on_cmd(AudioCmd::Finish { abort: true }, t0);
    assert!(!lanes.ptt());
    assert_eq!(engine.route(), Route::Reopening);
    // The stale Chatter is gone from the **first** quantum, not three
    // quanta later. The release flushes it while the output is still
    // closed, so the ring is emptied there and then; a 10 ms fade would
    // only make sense if there were a live stream to fade it out of, and
    // the one that was playing it died at the press. What the reopened
    // stream carries is the held Question and nothing else.
    let first = render(&mut engine, &mut out).expect("the output is open again");
    assert!(
        (first - 0.8).abs() < 1e-3,
        "the held Question, alone, from the first quantum: {first}"
    );
    assert_eq!(
        lanes.queued(Lane::Chatter),
        0,
        "and the stale Chatter went with the release (§7.2)"
    );
    let mut resumed = first;
    for _ in 0..3 {
        resumed = render(&mut engine, &mut out).expect("still open");
    }
    let resumed = resumed;
    assert!(
        (resumed - 0.8).abs() < 1e-3,
        "then the held Question plays on alone: the stale Chatter was \
         flushed at the release (§7.2), the Question was not ({resumed})"
    );

    // And the core was told, each time the route moved.
    let routes: Vec<Route> = from_core
        .try_iter()
        .filter_map(FromAudio::voice)
        .filter_map(|v| match v {
            VoiceIn::Route { route, .. } => Some(route),
            _ => None,
        })
        .collect();
    assert!(routes.contains(&Route::ScoOurs), "{routes:?}");
    assert!(routes.contains(&Route::Reopening), "{routes:?}");
}
