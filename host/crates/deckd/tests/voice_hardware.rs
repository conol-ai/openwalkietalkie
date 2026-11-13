#![cfg(target_os = "macos")]
//! The `--hardware` rows of §6, and nothing else.
//!
//! Every test here is `#[ignore]`d: CI runs `cargo test`, which skips them,
//! and `tools/host-test.sh --hardware` runs them with `--ignored`. Each one
//! **prints `skip` with a reason and passes** when the deck, the grant or
//! the opt-in is absent — §7's rule is that a missing grant is a skip and
//! never a failure, and a gate that went red on a Mac with no headset paired
//! would be turned off within a week.
//!
//! Two further opt-ins, above `--hardware` itself:
//!
//! * `OWT_HW_MIC=1` for anything that opens the microphone. Opening it can
//!   raise a TCC prompt for whichever app is *responsible* for this process
//!   (V9: measured as the terminal, not as us), and raising a system dialog
//!   is not something a test suite may do because somebody typed `--hardware`.
//! * The tone row plays **150 ms at −22 dBFS into the deck only**, never the
//!   system default output, and never changes any system volume.
use deck_model::VoiceInput;
use deckd::audio::coreaudio::{
    self, device_by_uid, discover_deck, is_alive, is_running_somewhere, name_of, nominal_rate,
    object_for_uid, uid_of, DECK_NAME,
};
use deckd::audio::engine::OutputSink;
use deckd::audio::lanes::{Lane, Lanes};
use deckd::audio::traits::{
    CaptureOpen, CaptureOpened, CaptureSink, CaptureSource, DeviceSide, StreamFaults,
};
use deckd::audio::tts::{chime_pcm, Chime};
use deckd::audio::LANE_RATE;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn skip(reason: &str) {
    println!("skip  {reason}");
}

/// The deck's UID pair, or `None` with a printed reason.
fn deck() -> Option<String> {
    match discover_deck() {
        Some(uid) => Some(uid),
        None => {
            skip("no Bluetooth :output device named \"OpenWalkieTalkie\" is connected");
            None
        }
    }
}

/// §6 / V11 / V12, off the real device: the UID is what cpal hands back,
/// the rates are what V8 measured, and the *name* is what keeps the
/// discovery off somebody's earbuds.
#[test]
#[ignore = "hardware: needs the deck connected as an audio device"]
fn hw_the_deck_is_found_by_uid_and_not_by_luck() {
    let Some(uid) = deck() else { return };
    println!("deck uid   {uid}");

    // Every Bluetooth `:output` on this machine, in CoreAudio's own order.
    let bluetooth: Vec<(String, String)> = coreaudio::devices()
        .into_iter()
        .filter(|d| coreaudio::is_bluetooth(*d))
        .filter_map(|d| Some((uid_of(d)?, name_of(d).unwrap_or_default())))
        .filter(|(u, _)| u.ends_with(":output"))
        .collect();
    println!("bluetooth :output devices, in order:");
    for (u, n) in &bluetooth {
        println!("  {u}  {n:?}");
    }
    assert!(
        !bluetooth.is_empty(),
        "the deck itself is one of them, so this cannot be empty"
    );
    // §7's rule is "Bluetooth, ends in `:output`, **and named OpenWalkieTalkie**".
    // The first two clauses alone are not enough whenever anything else is
    // paired, which on this machine they were not.
    assert_eq!(
        bluetooth
            .iter()
            .find(|(u, _)| *u == format!("{uid}:output"))
            .map(|(_, n)| n.as_str()),
        Some(DECK_NAME),
        "discovery must land on the deck by name"
    );
    if bluetooth[0].0 != format!("{uid}:output") {
        println!(
            "note  the first Bluetooth :output here is {:?} ({}), not the deck — \
             a UID-suffix-only rule would have picked it",
            bluetooth[0].1, bluetooth[0].0
        );
    }

    // V11: cpal finds both sides by UID, with no objc2 lookup.
    for side in [DeviceSide::Input, DeviceSide::Output] {
        let device = device_by_uid(&uid, side).expect("cpal finds it by UID");
        let id = cpal::traits::DeviceTrait::id(&device).expect("and gives the id back");
        println!("cpal {side:?}: {id}");
        assert!(id.to_string().starts_with("coreaudio:"));
    }

    // V8's rates, straight off the device.
    let input = object_for_uid(&format!("{uid}:input")).expect("the input object");
    let output = object_for_uid(&format!("{uid}:output")).expect("the output object");
    println!(
        "input  nominal {:?} alive {} running_somewhere {}",
        nominal_rate(input),
        is_alive(input),
        is_running_somewhere(input)
    );
    println!(
        "output nominal {:?} alive {} running_somewhere {}",
        nominal_rate(output),
        is_alive(output),
        is_running_somewhere(output)
    );
    assert_eq!(nominal_rate(input), Some(16_000), "V8: the input is 16 k");
}

/// §6's first two rows: press → first non-silent buffer (SCO bring-up), and
/// release → nominal 44100 (A2DP resume). The numbers, not adjectives.
///
/// **V2 is the whole point**: it reports the first *callback* and the first
/// *non-zero sample* separately, because they are about a second apart and
/// the second pass of the week-4 measurement walked straight into confusing
/// them.
#[test]
#[ignore = "hardware: opens the microphone; needs OWT_HW_MIC=1"]
fn hw_the_sco_bring_up_and_the_a2dp_resume_in_milliseconds() {
    let Some(uid) = deck() else { return };
    if std::env::var("OWT_HW_MIC").as_deref() != Ok("1") {
        skip("OWT_HW_MIC=1 not set (opening the mic may raise a TCC prompt)");
        return;
    }
    let output = object_for_uid(&format!("{uid}:output")).expect("the output object");
    let entry = nominal_rate(output);
    println!("output nominal at entry: {entry:?}");

    let faults = Arc::new(StreamFaults::new());
    let first_callback = Arc::new(AtomicU64::new(0));
    let first_nonzero = Arc::new(AtomicU64::new(0));
    let buffers = Arc::new(AtomicU64::new(0));
    let (cb, nz, n) = (
        first_callback.clone(),
        first_nonzero.clone(),
        buffers.clone(),
    );
    let t0 = Instant::now();
    let sink = CaptureSink::new(faults.clone(), move |_rate, samples| {
        let micros = t0.elapsed().as_micros() as u64;
        let _ = cb.compare_exchange(0, micros, Ordering::Relaxed, Ordering::Relaxed);
        n.fetch_add(1, Ordering::Relaxed);
        if samples.iter().any(|s| *s != 0.0) {
            let _ = nz.compare_exchange(0, micros, Ordering::Relaxed, Ordering::Relaxed);
        }
    });

    let mut capture = coreaudio::CpalCapture::new();
    let opened: CaptureOpened = match capture.open(
        &CaptureOpen {
            input: VoiceInput::Deck,
            device_uid: Some(uid.clone()),
            ask_deck: None,
        },
        sink,
    ) {
        Ok(opened) => opened,
        Err(e) => {
            // §7.8: a denied grant is a skip with a reason, never a failure
            // and never a prompt loop.
            skip(&format!("the microphone could not be opened: {e}"));
            return;
        }
    };
    println!("opened {:?} at {} Hz", opened.uid, opened.rate);
    assert_eq!(opened.rate, 16_000, "V8: {{16000}} is the only rate");

    let deadline = Instant::now() + Duration::from_secs(4);
    while first_nonzero.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let cb_ms = first_callback.load(Ordering::Relaxed) as f64 / 1000.0;
    let nz_ms = first_nonzero.load(Ordering::Relaxed) as f64 / 1000.0;
    println!(
        "first input callback : {cb_ms:8.2} ms   ({} buffers)",
        buffers.load(Ordering::Relaxed)
    );
    println!("first NON-ZERO sample: {nz_ms:8.2} ms");
    println!("output nominal during capture: {:?}", nominal_rate(output));

    // The release, and the A2DP resume timer that V1 measured at 2307 ms.
    let closed = Instant::now();
    capture.close();
    let mut resume_ms = None;
    while closed.elapsed() < Duration::from_secs(6) {
        if nominal_rate(output) == Some(44_100) {
            resume_ms = Some(closed.elapsed().as_secs_f64() * 1000.0);
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    match resume_ms {
        Some(ms) => println!("A2DP resume to 44100: {ms:8.2} ms after close"),
        None => println!(
            "A2DP resume to 44100: NOT within 6 s (nominal {:?})",
            nominal_rate(output)
        ),
    }
    if nz_ms > 0.0 {
        println!(
            "V2 holds: the first callback is {:.0}x earlier than the first real sample",
            nz_ms / cb_ms.max(0.001)
        );
        assert!(
            nz_ms >= cb_ms,
            "a non-zero sample cannot precede the callback that carried it"
        );
    } else {
        skip("no non-zero sample within 4 s — the mic may be muted or denied");
    }
}

/// The whole output half, end to end, into the deck alone: lanes → mixer →
/// cpal → the deck's `:output`. 150 ms, quiet, and it touches no system
/// setting.
#[test]
#[ignore = "hardware: plays 150 ms of quiet audio into the deck"]
fn hw_a_short_quiet_chime_reaches_the_deck() {
    let Some(uid) = deck() else { return };
    let output = object_for_uid(&format!("{uid}:output")).expect("the output object");
    let rate = nominal_rate(output).unwrap_or(LANE_RATE);
    println!("playing into {uid}:output at {rate} Hz");

    let lanes = Lanes::new();
    // −22 dBFS: the chime's own 0.2 peak, at `set speaker` 8. §7: nothing
    // plays at full gain into a headset someone may be wearing.
    lanes.set_master_step(8);
    let mut producer = lanes
        .take_producer(Lane::Chime)
        .expect("the chime producer");
    for s in chime_pcm(Chime::Ready, rate) {
        let _ = producer.push(s);
    }
    lanes.set_active(Lane::Chime, true);

    let mixer = lanes.take_mixer().expect("a mixer");
    let mut sink = coreaudio::CpalOutput::new(Some(uid.clone()));
    if let Err(e) = sink.open(rate, mixer, Arc::new(StreamFaults::new())) {
        skip(&format!("the deck's output could not be opened: {e}"));
        return;
    }
    let played = Instant::now();
    // Longer than it looks it needs to be, because a *cold* stream is the
    // case that matters. Week 4 measured this deck's A2DP resume at 2344 ms
    // (§11's week-4 rows), which is longer than the 2 s this loop used to
    // allow — so the row passed whenever a previous test had already warmed
    // the stream and failed the first time it ran on its own. A budget under
    // the number the same suite published is not a budget.
    while lanes.played(Lane::Chime) == 0 && played.elapsed() < Duration::from_secs(6) {
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(250));
    let frames = lanes.played(Lane::Chime);
    sink.close();
    println!("rendered {frames} frames from the chime lane");
    assert!(
        frames > 0,
        "the render callback never ran: the deck's output stream did not start"
    );
    // And §7.2's hand-off survived a real stream: the consumers came back.
    assert!(
        lanes.take_mixer().is_some(),
        "dropping the stream must return the lane consumers"
    );
}
