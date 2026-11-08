//! Original three-voice chiptunes and short effects, generated at 16 kHz.
//! Pulse lead, triangle bass and noise percussion; no samples or allocation.
//! The speaker owns the synthesizer. UI publishes one atomic command, so mute
//! and leaving the face cannot be stuck behind queued audio.
#[path = "sound/music.rs"]
mod music;

/// Full arrangement length at the synthesizer's 16 kHz sample rate.
pub fn music_loop_samples(track: u8) -> u32 {
    match track {
        1..=3 => music::STEP_SAMPLES[track as usize - 1] * music::STEPS,
        _ => 0,
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Effect {
    None,
    Start,
    Flap,
    Score,
    Shot,
    Hit,
    Over,
    Win,
    /// The Timer face's countdown ran out: four quick beeps a second.
    Alarm,
    /// A Pomodoro focus session ended: a rising three-note chime.
    FocusDone,
    /// A Pomodoro break ended: the same chime falling.
    BreakDone,
}

/// How long an alarm effect rings at most, in samples (30 s at 16 kHz).
/// The Timer face stops it sooner on any touch of the deck.
pub const RING_SAMPLES: u32 = 30 * 16_000;

/// One sample's tone of a ringing alarm effect: (Hz, amplitude). Zero
/// amplitude is a gap. Notes start and end on short ramps, so the gaps never
/// click.
fn ring(effect: u8, t: u32) -> (u32, i32) {
    const PEAK: i32 = 2000;
    // Linear ramps `up` samples in and `down` samples out of a note `len` long.
    let shape = |pos: u32, len: u32, up: u32, down: u32| -> i32 {
        PEAK * pos.min(up) as i32 / up as i32 * len.saturating_sub(pos).min(down) as i32 / down as i32
    };
    match effect {
        // 100 ms beeps every 150 ms, four of them, in each second.
        8 => {
            let at = t % 16_000;
            let (beep, pos) = (at / 2400, at % 2400);
            if beep < 4 && pos < 1600 {
                (1760, shape(pos, 1600, 40, 80))
            } else {
                (0, 0)
            }
        }
        // C6-E6-G6 (or falling), 200 ms each with the last held, every 2 s;
        // each note decays from its attack like a struck bell.
        9 | 10 => {
            const NOTES: [u32; 3] = [1047, 1319, 1568];
            let at = t % 32_000;
            let (note, pos, len) = match at {
                0..3200 => (0, at, 3200),
                3200..6400 => (1, at - 3200, 3200),
                6400..12_800 => (2, at - 6400, 6400),
                _ => return (0, 0),
            };
            let hz = NOTES[if effect == 9 { note } else { 2 - note }];
            let decay = (len - pos) as i32 * 1000 / len as i32;
            (hz, shape(pos, len, 40, 80) * decay / 1000)
        }
        _ => (0, 0),
    }
}
// track 0..3, mode 0..2, effect, 16-bit event serial, volume 0..10.
pub fn command(track: u8, mode: u8, effect: Effect, serial: u16, volume: u8) -> u32 {
    u32::from(track.min(3))
        | (u32::from(mode.min(2)) << 2)
        | ((effect as u32) << 4)
        | (u32::from(serial) << 8)
        | (u32::from(volume.min(10)) << 24)
}
pub fn enabled(word: u32) -> bool {
    word & 12 != 0 && (word >> 24) & 15 != 0
}
// Frequencies for C4..B5. Rests are zero. These melodies are original.
const HZ: [u16; 24] = [
    262, 277, 294, 311, 330, 349, 370, 392, 415, 440, 466, 494, 523, 554, 587, 622, 659, 698, 740,
    784, 831, 880, 932, 988,
];
#[derive(Default)]
pub struct Synth {
    track: u8,
    serial: u16,
    effect: u8,
    effect_left: u32,
    effect_len: u32,
    sample: u32,
    music: music::Step,
    lead_phase: u32,
    bass_phase: u32,
    fx_phase: u32,
    noise: u32,
    gain: i32,
}
fn phase(phase: &mut u32, hz: u32) -> u32 {
    *phase = phase.wrapping_add(hz * 65536 / 16000);
    *phase & 65535
}
impl Synth {
    pub fn pending(&self) -> bool {
        self.effect_left > 0
    }
    pub fn run(&mut self, word: u32, pcm: &mut [i16]) {
        let track = (word & 3) as u8;
        let mode = (word >> 2) & 3;
        let serial = (word >> 8) as u16;
        // Silence, game-over effects and a retry of the same game preserve
        // the song position. Otherwise short rounds only ever hear the intro.
        if track > 0 && track != self.track {
            self.track = track;
            self.sample = 0;
            self.lead_phase = 0;
            self.bass_phase = 0;
        }
        if serial != self.serial {
            self.serial = serial;
            self.effect = ((word >> 4) & 15) as u8;
            self.effect_len = match self.effect {
                1 => 3200,
                2 => 1120,
                3 => 2400,
                4 => 560,
                5 => 2400,
                6 => 8000,
                7 => 9600,
                8..=10 => RING_SAMPLES,
                _ => 0,
            };
            self.effect_left = self.effect_len;
            self.fx_phase = 0;
        }
        if !enabled(word) {
            self.effect_left = 0;
            self.gain = 0;
            pcm.fill(0);
            return;
        }
        let volume = ((word >> 24) & 15).min(10) as i32;
        for sample in pcm {
            let mut value = 0;
            self.noise = self.noise.wrapping_mul(1664525).wrapping_add(1013904223);
            if track > 0 && mode == 2 {
                let beat = music::STEP_SAMPLES[track as usize - 1];
                let at = self.sample % beat;
                if at == 0 {
                    self.music = music::step(track as usize - 1, self.sample / beat);
                }
                let note = self.music.lead;
                // Short attack and release soften edges, retaining the 8-bit pulse timbre.
                let env = at.min(80).min(beat.saturating_sub(at).min(240)) as i32;
                if note > 0 {
                    value += if phase(&mut self.lead_phase, u32::from(HZ[note as usize - 1]))
                        < self.music.pulse_width
                    {
                        self.music.pulse_high
                    } else {
                        self.music.pulse_low
                    } * env
                        / 80
                        * self.music.lead_level
                        / 100;
                }
                let bass = HZ[self.music.bass as usize - 1] / 4;
                let p = phase(&mut self.bass_phase, u32::from(bass)) as i32;
                value += ((p - 32768).abs() - 16384) * self.music.bass_level / 16384;
                if at < self.music.drum_len {
                    value += if self.noise & 0x80000000 != 0 {
                        self.music.drum_level
                    } else {
                        -self.music.drum_level
                    } * (self.music.drum_len - at) as i32
                        / self.music.drum_len as i32;
                }
                self.sample += 1;
                if self.sample == music_loop_samples(track) {
                    self.sample = 0;
                }
            }
            if self.effect_left > 0 && self.effect >= 8 {
                let t = self.effect_len - self.effect_left;
                let (hz, amp) = ring(self.effect, t);
                if amp > 0 {
                    value += if phase(&mut self.fx_phase, hz) < 32768 { amp } else { -amp };
                }
                self.effect_left -= 1;
            } else if self.effect_left > 0 {
                let t = self.effect_len - self.effect_left;
                let hz = match self.effect {
                    1 => [523, 659, 784, 1047][(t / 800).min(3) as usize],
                    2 => 450 + t / 2,
                    3 => {
                        if t < 1200 {
                            784
                        } else {
                            1047
                        }
                    }
                    4 => 1500u32.saturating_sub(t * 2).max(180),
                    5 => 180 + (self.noise & 127),
                    6 => 450u32.saturating_sub(t / 24).max(80),
                    7 => [523, 659, 784, 1047][(t / 2400).min(3) as usize],
                    _ => 0,
                };
                let amp = 1000 * self.effect_left.min(240).min(t.min(40)) as i32 / 40;
                let high = if self.effect == 5 {
                    self.noise & 0x80000000 != 0
                } else {
                    phase(&mut self.fx_phase, hz) < 32768
                };
                value += if high { amp } else { -amp };
                self.effect_left -= 1;
            }
            self.gain = (self.gain + 1).min(160);
            *sample = (value * volume * self.gain / 10 / 160).clamp(-4096, 4096) as i16;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mute_volume_and_sfx_only_are_silent_without_effects() {
        let mut synth = Synth::default();
        let mut pcm = [1; 320];
        for word in [
            0,
            command(1, 0, Effect::Shot, 1, 6),
            command(1, 2, Effect::Shot, 2, 0),
            command(1, 1, Effect::None, 3, 6),
        ] {
            synth.run(word, &mut pcm);
            assert!(pcm.iter().all(|&s| s == 0));
        }
    }
    #[test]
    fn alarms_ring_with_gaps_stay_bounded_and_stop_with_their_word() {
        for (effect, gap_at) in [(Effect::Alarm, 12_000), (Effect::FocusDone, 20_000), (Effect::BreakDone, 20_000)] {
            let mut synth = Synth::default();
            let word = command(0, 1, effect, 1, 10);
            let mut second = [0i16; 16_000];
            synth.run(word, &mut second);
            assert!(second.iter().all(|s| s.abs() < 4096), "{effect:?} must not hit the limiter");
            assert!(second[..4000].iter().any(|s| s.abs() > 1000), "{effect:?} sounds at once");
            assert!(synth.pending(), "{effect:?} rings on");
            // A gap in the pattern is silence, not a held level.
            let mut rest = [0i16; 320];
            let mut gap = Synth::default();
            let mut skip = std::vec![0i16; gap_at as usize];
            gap.run(word, &mut skip);
            gap.run(word, &mut rest);
            assert!(rest.iter().all(|&s| s == 0), "{effect:?} has quiet gaps");
            // The word going away silences it on the next block.
            synth.run(0, &mut rest);
            assert!(rest.iter().all(|&s| s == 0) && !synth.pending());
        }
        // The three sound different.
        let block = |effect| {
            let mut synth = Synth::default();
            let mut pcm = [0i16; 8000];
            synth.run(command(0, 1, effect, 1, 10), &mut pcm);
            pcm
        };
        assert_ne!(block(Effect::Alarm), block(Effect::FocusDone));
        assert_ne!(block(Effect::FocusDone), block(Effect::BreakDone));
    }

    #[test]
    fn an_alarm_rings_for_its_whole_length_then_ends() {
        let mut synth = Synth::default();
        let word = command(0, 1, Effect::Alarm, 1, 6);
        let mut pcm = [0i16; 16_000];
        for _ in 0..RING_SAMPLES / 16_000 {
            synth.run(word, &mut pcm);
        }
        assert!(!synth.pending(), "30 s and no more");
    }

    #[test]
    fn original_tracks_differ_and_stay_bounded() {
        let mut hashes = [0u64; 3];
        for track in 1..=3 {
            let mut s = Synth::default();
            let mut pcm = [0; 320];
            // Every section, then cross the loop boundary at maximum volume.
            for _ in 0..music_loop_samples(track) / 320 + 2 {
                s.run(command(track, 2, Effect::None, 0, 10), &mut pcm);
                for n in pcm {
                    assert!(n.abs() < 4096, "music must not hit the output limiter");
                    hashes[track as usize - 1] = hashes[track as usize - 1]
                        .wrapping_mul(31)
                        .wrapping_add(n as u64);
                }
            }
        }
        assert!(hashes.iter().all(|&h| h != 0));
        assert_ne!(hashes[0], hashes[1]);
        assert_ne!(hashes[1], hashes[2]);
    }
    #[test]
    fn arrangements_have_no_shorter_musical_period_and_all_notes_fit() {
        for track in 0..3 {
            let events: std::vec::Vec<_> = (0..music::STEPS)
                .map(|i| {
                    let s = music::step(track, i);
                    assert!(s.lead <= 24 && (1..=24).contains(&s.bass));
                    (s.lead, s.bass, s.pulse_width, s.lead_level, s.drum_len)
                })
                .collect();
            assert!(music_loop_samples(track as u8 + 1) >= 75 * 16000);
            for period in 1..events.len() {
                if events.len() % period == 0 {
                    assert!(
                        (period..events.len()).any(|i| events[i] != events[i % period]),
                        "track {track} repeats after only {period} steps"
                    );
                }
            }
        }
    }
    #[test]
    fn pause_game_over_and_sfx_only_preserve_music_position_for_retry() {
        let mut synth = Synth::default();
        let music = command(1, 2, Effect::None, 0, 6);
        synth.run(music, &mut [0; 7000]);
        let position = synth.sample;
        let phases = (synth.lead_phase, synth.bass_phase);
        for word in [
            0,
            command(0, 2, Effect::Over, 1, 6),
            command(1, 1, Effect::Flap, 2, 6),
        ] {
            synth.run(word, &mut [0; 320]);
            assert_eq!(synth.sample, position);
            assert_eq!((synth.lead_phase, synth.bass_phase), phases);
        }
        synth.run(music, &mut [0; 320]);
        assert_eq!(synth.sample, position + 320);
        synth.run(command(2, 2, Effect::None, 0, 6), &mut [0; 320]);
        assert_eq!(synth.sample, 320, "a different game starts its own theme");
    }
    #[test]
    fn audio_is_independent_of_chunk_size_across_sections_and_loop_wrap() {
        for track in 1..=3 {
            let word = command(track, 2, Effect::None, 0, 10);
            let length = music_loop_samples(track) as usize + 333;
            let mut whole = std::vec![0; length];
            Synth::default().run(word, &mut whole);
            let mut chunked = std::vec![0; length];
            let mut synth = Synth::default();
            for chunk in chunked.chunks_mut(317) {
                synth.run(word, chunk);
            }
            assert_eq!(synth.sample, 333);
            assert_eq!(whole, chunked);
        }
    }
    #[test]
    fn effects_finish_do_not_retrigger_and_mute_cancels() {
        for fx in [
            Effect::Start,
            Effect::Flap,
            Effect::Score,
            Effect::Shot,
            Effect::Hit,
            Effect::Over,
            Effect::Win,
        ] {
            let mut s = Synth::default();
            let mut pcm = [0; 320];
            let word = command(0, 1, fx, 1, 6);
            s.run(word, &mut pcm);
            assert!(pcm.iter().any(|&n| n != 0));
            for _ in 0..40 {
                s.run(word, &mut pcm);
            }
            assert!(!s.pending());
            assert!(pcm.iter().all(|&n| n == 0));
            s.run(command(0, 1, fx, 2, 6), &mut pcm);
            assert!(s.pending());
            s.run(0, &mut pcm);
            assert!(!s.pending());
            assert!(pcm.iter().all(|&n| n == 0));
        }
    }
}
