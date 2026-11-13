//! Local computer telemetry. One lightweight sampler, no subprocesses or admin
//! privileges; transient readings go through the normal coalescing P3 lane.
use crate::core::handle::{CoreHandle, CoreMsg};
use deck_proto::SystemStatus;
use std::time::Duration;

#[cfg(target_os = "macos")]
mod macos;

#[derive(Default)]
pub struct Sampler {
    previous_cpu: Option<[u32; 4]>,
    sequence: u32,
}
fn cpu_percent(previous: [u32; 4], current: [u32; 4]) -> Option<u8> {
    let delta = std::array::from_fn::<_, 4, _>(|i| u64::from(current[i].wrapping_sub(previous[i])));
    let total: u64 = delta.iter().sum();
    // Mach CPU_STATE_IDLE is slot 2; normalize across all cores to 0..100.
    (total > 0).then(|| ((total - delta[2]) * 100 / total).min(100) as u8)
}
fn mib(bytes: u64) -> u32 {
    (bytes / 1_048_576).min(u64::from(u32::MAX)) as u32
}
impl Sampler {
    /// Sampling stopped: the next sample starts the CPU's delta afresh
    /// rather than averaging over the pause (its CPU is unknown, as the
    /// first sample's is).
    pub fn pause(&mut self) {
        self.previous_cpu = None;
    }

    pub fn sample(&mut self) -> SystemStatus {
        self.sequence = self.sequence.wrapping_add(1);
        let mut status = SystemStatus {
            sample: self.sequence,
            ..SystemStatus::EMPTY
        };
        #[cfg(target_os = "macos")]
        {
            let cpu = macos::cpu_ticks();
            status.cpu = self
                .previous_cpu
                .zip(cpu)
                .and_then(|(old, new)| cpu_percent(old, new));
            self.previous_cpu = cpu;
            if let Some((used, total)) = macos::memory() {
                status.memory_used_mib = mib(used.min(total));
                status.memory_total_mib = mib(total);
            }
            if let Some((free, total)) = macos::disk() {
                status.disk_free_mib = mib(free.min(total));
                status.disk_total_mib = mib(total);
            }
            status.gpu = macos::gpu();
        }
        status
    }
}

/// How often the System face's numbers are sampled.
pub const SAMPLE_EVERY: Duration = Duration::from_secs(2);
/// How often the sampler looks at the stop flag and the gate.
const STEP: Duration = Duration::from_millis(100);

/// The sampler, until `stop`. It samples only while the gate says so
/// (`crate::dormancy`): the System face on the deck's strip and a deck
/// linked in the last ten minutes, because the face is its only reader.
/// It samples at once when that starts.
pub async fn run(core: CoreHandle, stop: tokio::sync::watch::Receiver<bool>) {
    let gate = core.adapters().watch_gate();
    // Native OS calls stay off both the reducer and async I/O threads.
    let _ = tokio::task::spawn_blocking(move || {
        let mut sampler = Sampler::default();
        // When the next sample is due; `None` while sampling is off, so the
        // first one after it comes on is at once.
        let mut next: Option<std::time::Instant> = None;
        while !*stop.borrow() && stop.has_changed().is_ok() {
            if gate.borrow().system {
                let now = std::time::Instant::now();
                if next.is_none_or(|at| now >= at) {
                    if !core.send_blocking(CoreMsg::SystemStatus(sampler.sample())) {
                        break;
                    }
                    next = Some(now + SAMPLE_EVERY);
                }
            } else if next.take().is_some() {
                sampler.pause();
            }
            std::thread::sleep(STEP);
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cpu_load_is_a_delta_normalized_across_cores() {
        assert_eq!(cpu_percent([0; 4], [20, 10, 70, 0]), Some(30));
        assert_eq!(
            cpu_percent([100, 100, 100, 0], [140, 120, 240, 0]),
            Some(30)
        );
        assert_eq!(cpu_percent([1; 4], [1; 4]), None);
        assert_eq!(
            cpu_percent([u32::MAX - 9, 0, 0, 0], [10, 0, 80, 0]),
            Some(20)
        );
    }

    /// After a pause (the face off the strip, or no deck for ten minutes)
    /// the first sample does not average the CPU over the whole pause.
    #[test]
    fn a_pause_starts_the_cpu_delta_afresh_and_the_numbering_goes_on() {
        let mut sampler = Sampler {
            previous_cpu: Some([1, 2, 3, 4]),
            sequence: 7,
        };
        sampler.pause();
        assert_eq!(sampler.previous_cpu, None);
        assert_eq!(sampler.sample().sample, 8);
    }
}
