//! Rev A's speaker: the SF32's audio DAC into the NS4150B class-D amplifier,
//! whose CTRL pin is PA20 on Rev A and PA1 on Rev B (100k pull-down: off
//! until driven; `board` passes the pin).
//!
//! A host speaker stream starts the audio PLL and the DAC at 48 kHz (the
//! HAL programs the codec's oversampling for 48 kHz whatever the rate, so
//! 16 kHz is upsampled 3x here rather than trusted to the DAC), enables the
//! amplifier, and plays frames as they arrive. Gaps play silence; END, or
//! 300 ms with nothing, ends the stream and powers everything down. While a
//! microphone stream is live the speaker stays silent: the deck is a
//! walkie-talkie and its own speaker would land in its own microphone.
//! Arcade music uses this same DAC owner and yields to incoming host speech.
//! No local samples enter the host queue or the microphone path.
use core::sync::atomic::Ordering;
use embassy_time::{with_timeout, Duration, Instant, Timer};
use sifli_hal::aud_pll::{AudPllFreq, AudioPll};
use sifli_hal::audio::{self, AudioDac, ChannelMode, DacConfig, SampleRate};
use sifli_hal::bind_interrupts;
use sifli_hal::gpio::{Level, Output};
use sifli_hal::pac;
use sifli_hal::peripherals::{AUDPRC, DMAC1_CH1};

use super::codec;
use super::policy::{SpeakerGate, Verdict};
use super::upsample::{Upsampler, FRAME_WORDS};
use super::voice::{self, Frame, SPEAKER_FRAMES};
use crate::logln;

bind_interrupts!(struct AudioIrqs {
    AUDPRC => audio::InterruptHandler;
});

/// DAC coarse volume (0-15; 6 is the HAL's 0 dB, and each step moves both
/// the AUDPRC and codec stages). One step down until the r6 boards get the
/// R4/R5 100k rework: with the assembled 22k inputs the NS4150B is driven
/// about 10 dB past clipping at full scale. Loudness is otherwise the host's
/// job (it scales the samples by the deck's speaker setting).
const VOLUME: u8 = 5;
/// Nothing for this long ends the stream.
const IDLE_MS: u32 = 300;
/// NS4150B: CTRL low for at least 550 us, one rising edge for mode 1, then
/// 100 ms before it passes audio (the native image's sequence).
const AMP_SETTLE_MS: u64 = 100;
/// DAC FIFO entries per millisecond: 48 kHz, two samples a word
/// (`upsample`).
const ENTRIES_PER_MS: usize = 24;
/// One 20 ms frame at the DAC rate: 960 samples in 480 words.
const CHUNK: usize = FRAME_WORDS;
/// The DMA ring: 80 ms, refilled 20 ms at a time. It is the jitter buffer,
/// and deep enough to ride out the UI holding the executor for a while.
const DMA_ENTRIES: usize = 4 * CHUNK;
/// When the ring is down to this much, a late frame gives way to silence.
const GUARD_MS: usize = 8;

static mut DMA_BUF: [u32; DMA_ENTRIES] = [0; DMA_ENTRIES];

pub struct Speaker {
    audprc: AUDPRC,
    dma: DMAC1_CH1,
    amp: Output<'static>,
}

impl Speaker {
    pub fn new(
        audprc: AUDPRC,
        dma: DMAC1_CH1,
        amp: impl sifli_hal::Peripheral<P = impl sifli_hal::gpio::Pin> + 'static,
    ) -> Self {
        Self {
            audprc,
            dma,
            amp: Output::new(amp, Level::Low),
        }
    }
}

fn muted() -> bool {
    crate::link::charging_only()
        || voice::CAPTURING.load(Ordering::Acquire) && !voice::LOOPBACK.load(Ordering::Acquire)
}

/// A DMA write that takes this long means the TX DMA has stopped.
const WRITE_TIMEOUT: Duration = Duration::from_millis(250);

#[embassy_executor::task]
pub async fn task(mut speaker: Speaker) {
    let mut gate = SpeakerGate::default();
    let mut synth = crate::ui::state::arcade::sound::Synth::default();
    let mut last_local = 0;
    let mut pending = None;
    loop {
        let frame = match pending.take() {
            Some(frame) => Some(frame),
            None if !crate::ui::state::arcade::sound::enabled(super::arcade::current()) => {
                use embassy_futures::select::{select, Either};
                match select(SPEAKER_FRAMES.receive(), super::arcade::CHANGED.wait()).await {
                    Either::First(frame) => Some(frame),
                    Either::Second(()) => None,
                }
            }
            None => with_timeout(Duration::from_millis(20), SPEAKER_FRAMES.receive())
                .await
                .ok(),
        };
        if let Some(frame) = frame {
            if muted() {
                continue;
            }
            let parsed = codec::parse(frame.as_bytes());
            if admit(&mut gate, &parsed, frame.usb)
                && matches!(
                    parsed,
                    codec::Frame::Start { .. } | codec::Frame::Audio { .. }
                )
            {
                pending = play(&mut speaker, &mut gate, Some(frame), &mut synth).await;
            }
        } else {
            let word = super::arcade::current();
            if crate::ui::state::arcade::sound::enabled(word)
                && (word & 3 != 0 || word != last_local)
                && !voice::CAPTURING.load(Ordering::Acquire)
            {
                pending = play(&mut speaker, &mut gate, None, &mut synth).await;
                last_local = super::arcade::current();
            } else if word == 0 {
                last_local = 0;
            }
        }
    }
}

/// Whether to act on a frame; a refused stream is logged once.
fn admit(gate: &mut SpeakerGate, frame: &codec::Frame, usb: bool) -> bool {
    match gate.admit_on(frame, usb) {
        Verdict::Play => true,
        Verdict::Drop => false,
        Verdict::Refuse => {
            if let codec::Frame::Start {
                stream,
                codec,
                rate_khz,
                frame_ms,
            } = *frame
            {
                logln!(
                    "voice: speaker stream {} refused: codec {}, {} kHz, {} ms",
                    stream,
                    codec,
                    rate_khz,
                    frame_ms
                );
            }
            false
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ending {
    End,
    Idle,
    Muted,
    Stalled,
    Preempted,
}

async fn play(
    speaker: &mut Speaker,
    gate: &mut SpeakerGate,
    first: Option<Frame>,
    synth: &mut crate::ui::state::arcade::sound::Synth,
) -> Option<Frame> {
    let local = first.is_none();
    #[cfg(feature = "audio-usb-rate-test")]
    let test_usb = first.as_ref().is_some_and(|frame| frame.usb);
    #[cfg(feature = "audio-usb-rate-test")]
    if test_usb { super::usb_test::begin(gate.rate_khz()); }
    let mut pending = None;
    voice::PLAYING.store(true, Ordering::Release);
    let started = Instant::now();
    // Blocks for about 13 ms (PLL calibration and DAC settle). The DAC is
    // unmuted and idle (silent) once this returns.
    let pll = AudioPll::new(AudPllFreq::Mhz49_152);
    let mut config = DacConfig::default();
    config.sample_rate = SampleRate::Hz48000;
    config.channel_mode = ChannelMode::Mono;
    config.volume = VOLUME;
    let mut dac = AudioDac::new(
        &mut speaker.audprc,
        &mut speaker.dma,
        &pll,
        AudioIrqs,
        config,
    );
    speaker.amp.set_low();
    // The SDK's 48 kHz XTAL path uses DAC CLK_DIV=1 and an AUDPRC
    // strobe divider of 1000 (48 MHz / 48 kHz). Keep both blocks on
    // that crystal clock before DMA starts: the pinned HAL's PLL/1024
    // strobe paired with its XTAL codec produces DAC noise and slow
    // playback on Rev A. Preserve oversampling and analog calibration.
    pac::AUDCODEC.dac_cfg().modify(|w| {
        w.set_clk_src_sel(false);
        w.set_clk_div(1);
    });
    pac::AUDPRC.cfg().modify(|w| w.set_stb_clk_sel(false));
    pac::AUDPRC.stb().modify(|w| w.set_dac_div(1000));
    logln!(
        "voice: speaker DAC_CFG=0x{:08x}",
        pac::AUDCODEC.dac_cfg().read().0
    );
    Timer::after_micros(600).await;
    speaker.amp.set_high();
    // Frames queue up behind the settle, which primes the jitter buffer.
    Timer::after_millis(AMP_SETTLE_MS).await;

    // The ring starts full (of zeros) and the DMA starts reading it at once,
    // so the stream is started only now: a writer a lap behind the DMA is an
    // overrun.
    // SAFETY: the one stream alive at a time borrows it; this task is the
    // buffer's only user.
    let buffer = unsafe { &mut *core::ptr::addr_of_mut!(DMA_BUF) };
    buffer.fill(0);
    let mut stream = dac.start_stream(buffer);
    // The ring's fill at the last write, and when that was: the DMA drains
    // it at exactly ENTRIES_PER_MS.
    let mut queued = DMA_ENTRIES;
    let mut wrote_at = Instant::now();

    let mut up = Upsampler::default();
    let mut pcm = [0i16; codec::FRAME_SAMPLES];
    let mut out = [0u32; CHUNK];
    let mut next = first;
    #[cfg(feature = "audio-debug")]
    let mut bench_phase = 0usize;
    #[cfg(feature = "audio-usb-rate-test")]
    let mut next_seq = 0u8;
    let mut quiet_ms = 0u32;
    let (mut frames, mut gaps, mut underruns) = (0u32, 0u32, 0u32);
    let ending = loop {
        let frame = match next.take() {
            Some(frame) => Some(frame),
            None if local => SPEAKER_FRAMES.try_receive().ok(),
            None => {
                // Wait for the host for as long as what is left in the ring
                // can cover.
                let drained = wrote_at.elapsed().as_millis() as usize * ENTRIES_PER_MS;
                let left = queued.saturating_sub(drained);
                let wait = (left / ENTRIES_PER_MS).saturating_sub(GUARD_MS);
                with_timeout(Duration::from_millis(wait as u64), SPEAKER_FRAMES.receive())
                    .await
                    .ok()
            }
        };
        if muted() || (local && voice::CAPTURING.load(Ordering::Acquire)) {
            break Ending::Muted;
        }
        // A refused frame counts as a gap: silence, never a starved ring.
        let parsed = frame
            .as_ref()
            .map(|f| codec::parse(f.as_bytes()))
            .filter(|parsed| admit(gate, parsed, frame.as_ref().is_some_and(|f| f.usb)));
        if local
            && matches!(
                parsed,
                Some(codec::Frame::Start { .. } | codec::Frame::Audio { .. })
            )
        {
            pending = frame;
            break Ending::Preempted;
        }
        let word = super::arcade::current();
        if local && !crate::ui::state::arcade::sound::enabled(word) {
            break Ending::Muted;
        }
        let mut local_end = false;
        let m = match parsed {
            Some(codec::Frame::Audio {
                mut state, data, seq: _seq, ..
            }) => {
                let n = codec::decode(&mut state, data, &mut pcm);
                quiet_ms = 0;
                frames += 1;
                #[cfg(feature = "audio-usb-rate-test")]
                {
                    if test_usb { super::usb_test::decoded(n, _seq == next_seq); }
                    next_seq = _seq.wrapping_add(1);
                }
                up.run_at(gate.rate_khz(), &pcm[..n], &mut out)
            }
            Some(codec::Frame::End { .. }) if !local => break Ending::End,
            Some(_) => continue,
            #[cfg(feature = "audio-debug")]
            None if voice::bench::HOLD.load(Ordering::Acquire) => {
                // Bench: the deck's own silence or tone, with no host stream.
                quiet_ms = 0;
                match voice::bench::TONE.load(Ordering::Acquire) {
                    0 => up.silence(&mut out),
                    peak => {
                        voice::bench::tone(&mut pcm, peak, &mut bench_phase);
                        up.run(&pcm, &mut out)
                    }
                }
            }
            None if local => {
                synth.run(word, &mut pcm);
                local_end = word & 3 == 0 && !synth.pending();
                up.run(&pcm, &mut out)
            }
            None => {
                quiet_ms += codec::FRAME_MS as u32;
                gaps += 1;
                if quiet_ms >= IDLE_MS {
                    break Ending::Idle;
                }
                up.silence(&mut out)
            }
        };
        match with_timeout(WRITE_TIMEOUT, stream.write(&out[..m])).await {
            Ok(Ok(free)) => queued = DMA_ENTRIES - free,
            // The DMA lapped the writer (the executor was held past the
            // ring). The HAL re-arms the ring a lap ahead, which replays
            // up to 80 ms of old samples once; the stream carries on.
            Ok(Err(_)) => {
                underruns += 1;
                queued = DMA_ENTRIES;
            }
            Err(_) => break Ending::Stalled,
        }
        wrote_at = Instant::now();
        if local_end {
            break Ending::End;
        }
    };
    if matches!(ending, Ending::End | Ending::Idle) {
        // A full ring of silence: once it is accepted, everything before it
        // has gone to the DAC, and the ring holds nothing stale to replay.
        // A microphone opening meanwhile cuts it short.
        for _ in 0..DMA_ENTRIES / CHUNK {
            if muted() {
                break;
            }
            let m = up.silence(&mut out);
            if !matches!(
                with_timeout(WRITE_TIMEOUT, stream.write(&out[..m])).await,
                Ok(Ok(_))
            ) {
                break;
            }
        }
    }
    speaker.amp.set_low();
    drop(stream);
    drop(dac);
    drop(pll);
    // The PLL's drop clears the audio clock buffer the microphone may share.
    if voice::CAPTURING.load(Ordering::Acquire) {
        pac::PMUC.hxt_cr1().modify(|w| w.set_buf_aud_en(true));
    }
    voice::PLAYING.store(false, Ordering::Release);
    #[cfg(feature = "audio-usb-rate-test")]
    if test_usb { super::usb_test::finish(gaps, underruns, ending == Ending::End); }
    gate.finish();
    logln!(
        "voice: speaker stream {} after {} ms: {} frames, {} gaps, {} underruns",
        match ending {
            Ending::End => "ended",
            Ending::Idle => "timed out",
            Ending::Muted => "cut by the microphone",
            Ending::Stalled => "abandoned (DAC DMA stalled)",
            Ending::Preempted => "yielded to host speech",
        },
        started.elapsed().as_millis(),
        frames,
        gaps,
        underruns
    );
    pending
}
