//! **B10, on the only thread where it is true.**
//!
//! `harness = false`, so this file's `fn main` *is* the process's main
//! thread. That is not a convenience: `dispatch_async` to the main queue
//! runs on the main thread and nowhere else, and libtest runs every
//! `#[test]` on a worker — so the same three assertions written as `#[test]`
//! pass vacuously at best and fail at worst. They failed: the first version
//! of this file was a `mod tests` in `audio/mainloop.rs` and all three rows
//! went red, because the worker turned its own run loop and the main queue
//! never drained. That is V13's measurement reproduced by accident (a
//! worker driving its own run loop → 0 buffers), and it is the reason the
//! daemon has to give `main` away rather than spawn a "run loop thread".
//!
//! It prints its own results: a `harness = false` target has no libtest
//! summary, and forging one would inflate the gate's count.
#![cfg(target_os = "macos")]

use deckd::audio::apple::{AppleStt, AppleTts};
use deckd::audio::mainloop::{call, is_main_thread, post, turn};
use deckd::audio::traits::{
    OnDevice, SttEngine, SttEvent, SttStart, TtsChunk, TtsEngine, TtsRequest,
};
use deckd::audio::FromAudio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn pump_until(done: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() && Instant::now() < deadline {
        turn(0.05);
    }
    done()
}

fn row(name: &str, ok: bool, failures: &mut usize) {
    if ok {
        println!("ok    mainloop {name}");
    } else {
        println!("FAIL  mainloop {name}");
        *failures += 1;
    }
}

/// Opt-in real-framework regression: no microphone, playback, or messages
/// sent to an agent. Generate three sentences, feed them in real time with
/// three-second pauses, then release PTT while the last phrase is pending.
fn speech_with_pauses() -> String {
    let tts = AppleTts::new();
    let voice = tts
        .voices()
        .into_iter()
        .find(|voice| voice.locale == "en-US" && voice.name == "Samantha")
        .expect("install the Samantha en-US voice for this optional regression");
    let mut recording = Vec::new();
    let mut rate = 0;
    for phrase in [
        "Please keep the first sentence.",
        "Then add the second sentence.",
        "Finally keep all three sentences.",
    ] {
        let job = tts
            .synthesize(&TtsRequest {
                text: phrase.into(),
                voice: Some(voice.id.clone()),
                rate: 0.45,
            })
            .expect("synthesize a test phrase");
        let format = job.format();
        assert_eq!(format.channels, 1);
        if rate == 0 {
            rate = format.rate;
        }
        assert_eq!(rate, format.rate);
        if !recording.is_empty() {
            recording.resize(recording.len() + rate as usize * 3, 0.0);
        }
        let chunks = job.chunks();
        loop {
            match chunks
                .recv_timeout(Duration::from_secs(10))
                .expect("TTS chunk")
            {
                TtsChunk::Pcm(samples) => recording.extend(samples),
                TtsChunk::End => break,
                TtsChunk::Error(error) => panic!("TTS failed: {error}"),
            }
        }
    }
    let mut session = AppleStt::new()
        .start(&SttStart {
            locale: "en-US".into(),
            contextual: Vec::new(),
            allow_server: false,
        })
        .expect("start on-device STT");
    let events = session.events();
    let mut preview_with_all_phrases = false;
    for chunk in recording.chunks(rate as usize / 10) {
        session.feed(chunk, rate);
        std::thread::sleep(Duration::from_millis(100));
        for event in events.try_iter() {
            match event {
                SttEvent::Partial(text) => {
                    preview_with_all_phrases |= contains_all_phrases(&text);
                }
                SttEvent::Error { code, message } => panic!("STT failed: {code}: {message}"),
                SttEvent::Final(_) => panic!("STT ended before the simulated PTT release"),
            }
        }
    }
    let final_text =
        deckd::audio::block_on_until(session.finish(), Instant::now() + Duration::from_secs(10))
            .expect("STT final deadline")
            .expect("STT final result");
    assert!(
        preview_with_all_phrases,
        "preview lost a phrase across a pause"
    );
    assert!(
        contains_all_phrases(&final_text),
        "incomplete final: {final_text}"
    );
    for word in ["first", "second", "finally"] {
        assert_eq!(
            final_text.to_lowercase().matches(word).count(),
            1,
            "duplicated phrase: {final_text}"
        );
    }
    final_text
}

fn contains_all_phrases(text: &str) -> bool {
    let text = text.to_lowercase();
    matches!(
        (text.find("first sentence"), text.find("second sentence"), text.find("all three")),
        (Some(first), Some(second), Some(third)) if first < second && second < third
    )
}

fn main() {
    let mut failures = 0usize;

    // The premise. If this is false every row below is meaningless, so it is
    // a row of its own rather than an assumption.
    row("this is the main thread", is_main_thread(), &mut failures);

    // V13: work posted from a worker runs on **main**.
    let ran_on: Arc<Mutex<Option<std::thread::ThreadId>>> = Arc::new(Mutex::new(None));
    let count = Arc::new(AtomicUsize::new(0));
    let (seen, n) = (ran_on.clone(), count.clone());
    std::thread::spawn(move || {
        post(move || {
            *seen.lock().unwrap() = Some(std::thread::current().id());
            n.fetch_add(1, Ordering::SeqCst);
        });
    })
    .join()
    .unwrap();

    // Nothing runs until the run loop turns. That is exactly the state B10
    // found the daemon in: `main` owned the tokio runtime and the signal
    // handlers, so the queue existed and never drained.
    row(
        "a posted job waits for the run loop",
        count.load(Ordering::SeqCst) == 0,
        &mut failures,
    );
    let here = std::thread::current().id();
    let drained = pump_until(|| count.load(Ordering::SeqCst) == 1);
    row(
        "a posted job runs once the loop turns",
        drained,
        &mut failures,
    );
    row(
        "and it runs on main, not on the poster",
        *ran_on.lock().unwrap() == Some(here),
        &mut failures,
    );

    // §2.3's `dispatch_async(main_queue)` + a oneshot back.
    let answer = call(|| 6 * 7);
    let mut got = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    while got.is_none() && Instant::now() < deadline {
        turn(0.05);
        got = answer.try_recv().ok();
    }
    row(
        "a call hands its answer back",
        got == Some(42),
        &mut failures,
    );

    // Abandoned: a PTT session aborted while main is inside a TCC prompt.
    // The bounded(1) send fails; the main thread must not block on it.
    let ran = Arc::new(AtomicUsize::new(0));
    let n = ran.clone();
    drop(call(move || {
        n.fetch_add(1, Ordering::SeqCst);
        "nobody is listening"
    }));
    row(
        "an abandoned call still runs and does not block main",
        pump_until(|| ran.load(Ordering::SeqCst) == 1),
        &mut failures,
    );

    // A panic in posted ObjC work must not unwind into libdispatch, which
    // aborts with no message and no thread name.
    let after = Arc::new(AtomicUsize::new(0));
    let n = after.clone();
    // The hook is silenced only for this one row: the panic is the point,
    // and its backtrace on stderr would look like a failure to a reader of
    // the gate log.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    post(|| panic!("a voice that is not installed"));
    post(move || {
        n.fetch_add(1, Ordering::SeqCst);
    });
    let survived = pump_until(|| after.load(Ordering::SeqCst) == 1);
    std::panic::set_hook(previous);
    row(
        "a panicking job does not take the run loop with it",
        survived,
        &mut failures,
    );

    // ---- the Apple engines, on the only thread that can drive them -----
    //
    // V14: on-device STT and TTS need **no TCC grant**, so these rows are
    // not hardware rows — they run wherever the frameworks do. Nothing here
    // plays audio: `writeUtterance:toBufferCallback:` renders to buffers.

    // Asking an engine about itself from main must not deadlock. It would
    // have: `on_main` posts and waits, and the queue it posts to drains on
    // the thread that is waiting.
    let voices = AppleTts::new().voices();
    row(
        "AppleTts lists its voices from main without deadlocking",
        !voices.is_empty(),
        &mut failures,
    );
    let enhanced_en_us = voices
        .iter()
        .filter(|v| v.enhanced && v.locale == "en-US")
        .count();
    println!(
        "      {} voices, {} enhanced, {} en-US, {} enhanced en-US",
        voices.len(),
        voices.iter().filter(|v| v.enhanced).count(),
        voices.iter().filter(|v| v.locale == "en-US").count(),
        enhanced_en_us
    );
    println!(
        "      identifiers naming a downloaded tier: {}",
        voices
            .iter()
            .filter(|v| v.id.contains(".enhanced.") || v.id.contains(".premium."))
            .count()
    );
    // §7.7's `default_pool` is "4 distinct **enhanced** en-US voices present
    // on the machine, else the kind default". This prints which of those two
    // it is here rather than asserting a number: whether enhanced voices are
    // downloaded is a System Settings choice, not a property of the code.
    if enhanced_en_us < 4 {
        println!(
            "note  §7.7's default_pool cannot be filled from enhanced en-US \
             voices here ({enhanced_en_us} of 4); the kind default is the live path"
        );
    }

    // Week 6, M5: `voice.engines` is answered from a report `audio::spawn`
    // **posts** to main, because reading it is a framework enumeration and
    // the core thread may never park on the run loop. This is that exact
    // post — the job `spawn` queues, run through the same pump — and it is
    // the only place the produce half can be exercised: libtest owns no
    // main queue, so a `#[test]` for it would drain nothing.
    let (engines_tx, engines) = crossbeam_channel::unbounded::<FromAudio>();
    post(move || {
        let (stt, tts) = deckd::audio::apple::engines();
        let _ = engines_tx.send(FromAudio::Engines { stt, tts });
    });
    let mut reported = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while reported.is_none() && Instant::now() < deadline {
        turn(0.05);
        reported = engines.try_recv().ok();
    }
    match reported {
        Some(FromAudio::Engines { stt, tts }) => {
            println!(
                "      voice.engines: stt {} ({} locales, on_device {}), tts {} ({} locales)",
                stt.name,
                stt.locales.len(),
                stt.on_device,
                tts.name,
                tts.locales.len()
            );
            row(
                "the engines report `audio::spawn` posts reaches the core's channel",
                stt.name == "apple"
                    && tts.name == "apple"
                    && !stt.locales.is_empty()
                    && !tts.locales.is_empty(),
                &mut failures,
            );
        }
        _ => println!("skip  the engines report did not land on this machine"),
    }

    // §7.6's `-32009`, against the real recogniser. en-US is on-device on
    // this machine; zh-CN is supported but server-only ("No Assistant asset
    // for language").
    let stt = AppleStt::new();
    let en = stt.on_device("en-US");
    let zh = stt.on_device("zh-CN");
    println!("      on_device(en-US) = {en:?}, on_device(zh-CN) = {zh:?}");
    row(
        "AppleStt answers on-device support per locale",
        en == OnDevice::Yes && zh != OnDevice::Yes,
        &mut failures,
    );

    // V13 and V16 together: the synthesis runs on a worker (as `deckd-tts`
    // does), the buffers are delivered on main, and the **format is known
    // before any PCM is read**.
    let (done_tx, done) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let job = AppleTts::new().synthesize(&TtsRequest {
            text: "bot needs a decision".into(),
            voice: None,
            rate: 0.5,
        });
        let _ = done_tx.send(match job {
            Ok(job) => {
                let format = job.format();
                let chunks = job.chunks();
                let mut frames = 0usize;
                let mut ended = false;
                let deadline = Instant::now() + Duration::from_secs(20);
                while !ended && Instant::now() < deadline {
                    match chunks.recv_timeout(Duration::from_millis(250)) {
                        Ok(TtsChunk::Pcm(pcm)) => frames += pcm.len(),
                        Ok(TtsChunk::End) => ended = true,
                        Ok(TtsChunk::Error(_)) | Err(_) => break,
                    }
                }
                Some((format, frames, ended))
            }
            Err(_) => None,
        });
    });
    let mut synth = None;
    let deadline = Instant::now() + Duration::from_secs(25);
    while synth.is_none() && Instant::now() < deadline {
        turn(0.05);
        synth = done.try_recv().ok();
    }
    match synth.flatten() {
        Some((format, frames, ended)) => {
            println!("      AVSpeech format {format:?}, {frames} samples, end {ended}");
            row(
                "AVSpeech renders through the main run loop",
                frames > 0 && ended,
                &mut failures,
            );
            row(
                "and its format is not the lane format (V16: it must be resampled)",
                format.rate != 44_100 && format.channels >= 1,
                &mut failures,
            );
        }
        None => {
            println!("skip  AVSpeech produced no buffers on this machine");
        }
    }

    if std::env::var("OWT_TEST_STT_PAUSES").as_deref() == Ok("1") {
        let (done_tx, done) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let text = speech_with_pauses();
            let _ = done_tx.send(text);
        });
        let deadline = Instant::now() + Duration::from_secs(90);
        let result = loop {
            turn(0.05);
            match done.try_recv() {
                Ok(text) => break Some(text),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break None,
                Err(std::sync::mpsc::TryRecvError::Empty) if Instant::now() < deadline => {}
                Err(std::sync::mpsc::TryRecvError::Empty) => break None,
            }
        };
        if let Some(text) = &result {
            println!("      pause regression final: {text}");
        }
        row(
            "AppleStt preserves speech across pauses",
            result.is_some(),
            &mut failures,
        );
        if worker.is_finished() {
            let _ = worker.join();
        }
    } else {
        println!("skip  real STT pause regression (set OWT_TEST_STT_PAUSES=1)");
    }

    println!("mainloop result: {} failed", failures);
    if failures > 0 {
        std::process::exit(1);
    }
}
