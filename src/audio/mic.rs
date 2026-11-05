//! The microphone task: one stream per request, START, AUDIO every frame,
//! END. It only captures while a host is listening on some link (the mic
//! characteristic's subscription over BLE, LISTEN over the cable), binds
//! the stream to that link, and stops with it.
use core::sync::atomic::Ordering;
use embassy_time::{with_timeout, Duration, Instant};

use super::codec;
use super::pdm::{self, Pdm};
use super::policy::{Live, Owner};
use super::voice::{self, Frame, MicRequest, MIC, MIC_FRAMES};
use crate::link::Link;
use crate::logln;

/// A frame's samples arrive every 20 ms at most; this long without one
/// means the PDM DMA has stopped.
const READ_TIMEOUT: Duration = Duration::from_millis(200);

#[embassy_executor::task]
pub async fn task(mut pdm: Pdm) {
    let mut stream: u8 = 0;
    loop {
        // A Stop with no stream live is nothing to do.
        let MicRequest::Start { mode, limit_s, host } = MIC.receive().await else {
            continue;
        };
        let link = voice::audio_link();
        if link.is_none() && !local() {
            logln!("voice: microphone requested but no host is listening");
            continue;
        }
        stream = stream.wrapping_add(1);
        capture(&mut pdm, stream, mode, limit_s, Owner { host }, link).await;
    }
}

/// Bench only: this capture records into RAM (`voice::bench`).
fn local() -> bool {
    #[cfg(feature = "audio-debug")]
    return voice::bench::LOCAL.load(Ordering::Acquire);
    #[cfg(not(feature = "audio-debug"))]
    false
}

async fn capture(
    pdm: &mut Pdm,
    stream: u8,
    mode: u8,
    limit_s: u8,
    mut owner: Owner,
    link: Option<Link>,
) {
    let local = local();
    // A local capture has no link; the index is never used for it.
    let link = link.unwrap_or(Link::Ble);
    if !local && link == Link::Ble {
        voice::ACTIVITY.signal(());
    }
    voice::LOOPBACK.store(mode == codec::MIC_LOOPBACK, Ordering::Release);
    voice::CAPTURING.store(true, Ordering::Release);
    let started = Instant::now();
    let mut deadline = started + Duration::from_secs(u64::from(limit_s.max(1)));
    // START goes out before the DMA is armed: waiting on a full queue here
    // must not cost the ring.
    if !local {
        push(link, start(stream)).await;
    }
    let mut session = pdm.start().await;

    let mut state = codec::State::default();
    let mut seq: u8 = 0;
    let mut pcm = [0i16; codec::FRAME_SAMPLES];
    let (mut dropped, mut overruns) = (0u32, 0u32);
    let mut peak = 0i16;
    // Requests, the deadline and the subscription are looked at between
    // frames, never by cancelling a read halfway through a frame.
    let reason = loop {
        if let Ok(request) = MIC.try_receive() {
            match owner.request(request) {
                Live::End(reason) => break reason,
                Live::Continue { extend_s: Some(s) } => {
                    deadline = Instant::now() + Duration::from_secs(u64::from(s));
                }
                Live::Continue { extend_s: None } => {}
            }
        }
        if Instant::now() >= deadline {
            break codec::END_LIMIT;
        }
        if !local && !voice::subscribed(link) {
            break codec::END_ERROR;
        }
        let samples = if local {
            codec::FRAME_SAMPLES
        } else {
            voice::samples_per_frame(link).max(2)
        };
        match with_timeout(READ_TIMEOUT, session.read(&mut pcm[..samples])).await {
            Ok(Ok(())) => {
                let frame_peak = pcm[..samples].iter().fold(0i16, |p, s| p.max(s.saturating_abs()));
                peak = peak.max(frame_peak);
                // The PTT overlay's recording waveform follows this.
                crate::ui::meter::push(frame_peak as u16, crate::control::now_ms());
                #[cfg(feature = "audio-debug")]
                if local {
                    if voice::bench::record(&pcm[..samples]) {
                        break codec::END_LIMIT;
                    }
                    continue;
                }
                let mut bytes = [0u8; codec::MAX_FRAME];
                let n = codec::audio_frame(stream, seq, &mut state, &pcm[..samples], &mut bytes);
                seq = seq.wrapping_add(1);
                // Never wait for the link here: the PDM ring would overrun.
                // A full queue means the link is too slow; the frame is lost
                // and the host hears the gap in `seq`.
                if let Some(frame) = Frame::new(&bytes[..n]) {
                    if MIC_FRAMES[link.index()].try_send(frame).is_err() {
                        dropped += 1;
                    }
                }
            }
            // The executor was held past the ring: the HAL ring has reset
            // itself and up to a ring's worth of samples is gone. Say so in
            // `seq` (in this stream's frames) and carry on; the press is
            // still held.
            Ok(Err(pdm::Error::Overrun)) => {
                overruns += 1;
                seq = seq.wrapping_add((pdm::RING_SAMPLES / samples) as u8);
            }
            Err(_) => {
                logln!("voice: microphone DMA stalled");
                break codec::END_ERROR;
            }
        }
    };
    let mut end = [0u8; 4];
    let n = codec::end_frame(stream, seq, reason, &mut end);
    if let Some(frame) = Frame::new(&end[..n]).filter(|_| !local) {
        push(link, frame).await;
    }
    #[cfg(feature = "audio-debug")]
    if local {
        voice::bench::finish();
    }
    session.stop().await;
    voice::CAPTURING.store(false, Ordering::Release);
    voice::LOOPBACK.store(false, Ordering::Release);
    logln!(
        "voice: microphone stream {} over {:?} ended (reason {}) after {} ms: {} frames, {} dropped, {} overruns, peak {}",
        stream,
        link,
        reason,
        started.elapsed().as_millis(),
        seq,
        dropped,
        overruns,
        peak
    );
}

fn start(stream: u8) -> Frame {
    let mut bytes = [0u8; 5];
    let n = codec::start_frame(stream, &mut bytes);
    Frame::new(&bytes[..n]).unwrap_or_else(|| unreachable!())
}

/// START and END must not be lost to a momentarily full queue, but must not
/// hang on a dead link either.
async fn push(link: Link, frame: Frame) {
    let _ = with_timeout(Duration::from_millis(200), MIC_FRAMES[link.index()].send(frame)).await;
}
