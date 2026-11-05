//! The deck microphone's loudness over time, for the PTT overlay's
//! recording timeline.
//!
//! The firmware's mic task pushes each frame's peak; the meter folds them
//! into one bar per [`BAR_MS`] of audio (the loudest frame of the slice) and
//! keeps the last [`RING`] bars. The overlay remembers [`count`] when a press
//! starts and draws the bars committed since, one column each. A consumer
//! without the deck's own microphone (the native build, the host tests, the
//! preview, or a press captured by the Mac's input) never commits a bar, and
//! the timeline shows silence.
//!
//! Atomics only: the writer is the audio task (one writer, so its
//! read-modify-writes need no CAS), the reader the UI loop, and a torn read
//! is one bar drawn a frame late.

use core::sync::atomic::{AtomicU32, AtomicU8, Ordering};

/// One timeline column per this much audio.
pub const BAR_MS: u32 = 100;
/// Bars kept: 12.8 s, more than the timeline shows at once.
pub const RING: usize = 128;
/// A slice longer than this between frames is a new recording, not a gap
/// to fill.
const RESTART_MS: u32 = 1_000;

static BARS: [AtomicU8; RING] = [const { AtomicU8::new(0) }; RING];
static COUNT: AtomicU32 = AtomicU32::new(0);
static SLICE_MAX: AtomicU8 = AtomicU8::new(0);
static SLICE_START: AtomicU32 = AtomicU32::new(0);
static SLICE_OPEN: AtomicU8 = AtomicU8::new(0);

/// A frame's peak on a log scale: 16 per octave, so 0..=239 spans the
/// 96 dB of 16-bit audio.
#[must_use]
pub fn level(peak: u16) -> u8 {
    if peak == 0 {
        return 0;
    }
    let bits = 16 - peak.leading_zeros();
    let frac = (u32::from(peak) << (16 - bits) >> 11) & 0xF;
    (((bits - 1) * 16 + frac).min(255)) as u8
}

/// One frame of the deck's microphone.
pub fn push(peak: u16, now_ms: u32) {
    let level = level(peak);
    let start = SLICE_START.load(Ordering::Relaxed);
    let open = SLICE_OPEN.load(Ordering::Relaxed) != 0;
    let age = now_ms.wrapping_sub(start);
    if !open || age > RESTART_MS {
        SLICE_START.store(now_ms, Ordering::Relaxed);
        SLICE_MAX.store(level, Ordering::Relaxed);
        SLICE_OPEN.store(1, Ordering::Relaxed);
        return;
    }
    let max = SLICE_MAX.load(Ordering::Relaxed).max(level);
    if age < BAR_MS {
        SLICE_MAX.store(max, Ordering::Relaxed);
        return;
    }
    let n = COUNT.load(Ordering::Relaxed);
    BARS[n as usize % RING].store(max, Ordering::Relaxed);
    COUNT.store(n.wrapping_add(1), Ordering::Release);
    // Keep the cadence of the audio, not of the frames.
    SLICE_START.store(start.wrapping_add(BAR_MS), Ordering::Relaxed);
    SLICE_MAX.store(level, Ordering::Relaxed);
}

/// Bars committed so far, ever (a press remembers this at its start).
#[must_use]
pub fn count() -> u32 {
    COUNT.load(Ordering::Acquire)
}

/// Bar `index` (as counted by [`count`]), if it is still in the ring.
#[must_use]
pub fn bar(index: u32) -> Option<u8> {
    let count = count();
    (index < count && count - index <= RING as u32).then(|| BARS[index as usize % RING].load(Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_are_logarithmic_and_monotonic() {
        assert_eq!(level(0), 0);
        assert_eq!(level(1), 0);
        assert_eq!(level(32_767), 239);
        let mut last = 0;
        for peak in [3u16, 40, 300, 2_000, 9_000, 30_000] {
            let l = level(peak);
            assert!(l > last, "{peak} -> {l}");
            last = l;
        }
    }

    #[test]
    fn frames_fold_into_one_bar_per_slice_holding_the_loudest() {
        let base = count();
        let t = 50_000;
        // 20 ms frames: the first opens a slice, five more close it.
        for (i, peak) in [100u16, 9_000, 200, 300, 100, 50].into_iter().enumerate() {
            push(peak, t + i as u32 * 20);
        }
        assert_eq!(count(), base + 1);
        assert_eq!(bar(base), Some(level(9_000)));
        assert_eq!(bar(base + 1), None, "not committed yet");
        // A long gap starts a new recording instead of a run of silence.
        push(100, t + 5_000);
        assert_eq!(count(), base + 1);
    }
}
