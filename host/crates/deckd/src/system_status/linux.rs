//! The System face's numbers on Linux: `/proc/stat` and `/proc/meminfo`,
//! `statvfs` of the root file system, and the DRM drivers' own GPU load.
//! Files and one syscall only, as the macOS sampler is native calls only.
//! A GPU whose driver reports no load (Intel's i915, NVIDIA's blob) is
//! unavailable, never an invented 0%; with several, the busiest one.
use std::{mem::MaybeUninit, path::Path};

/// `/proc/stat`'s aggregate `cpu` line as the macOS ticks are laid out
/// (`cpu_percent` reads slot 2 as idle): user + nice, system and the time
/// spent on interrupts and for other guests, idle + iowait, and 0. The
/// counters are cumulative jiffies; truncating them to `u32` is what the
/// delta's `wrapping_sub` already expects of Mach's.
pub(super) fn parse_cpu(stat: &str) -> Option<[u32; 4]> {
    let line = stat.lines().find(|l| l.starts_with("cpu "))?;
    let v: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .map(|f| f.parse().ok())
        .collect::<Option<_>>()?;
    let at = |i: usize| v.get(i).copied().unwrap_or(0);
    if v.len() < 4 {
        return None;
    }
    // user nice system idle iowait irq softirq steal (guest time is already in user).
    let busy_user = at(0) + at(1);
    let busy_system = at(2) + at(5) + at(6) + at(7);
    let idle = at(3) + at(4);
    Some([busy_user as u32, busy_system as u32, idle as u32, 0])
}

/// (used, total) bytes: used is what the kernel says cannot be had without
/// swapping (`MemTotal - MemAvailable`), the counterpart of the macOS app,
/// wired and compressed pages; the page cache counts as free on both.
pub(super) fn parse_memory(meminfo: &str) -> Option<(u64, u64)> {
    let field = |name: &str| {
        meminfo.lines().find_map(|l| {
            let rest = l.strip_prefix(name)?.strip_prefix(':')?;
            rest.split_whitespace().next()?.parse::<u64>().ok()
        })
    };
    let total = field("MemTotal")?.checked_mul(1024)?;
    let available = field("MemAvailable")?.saturating_mul(1024);
    (total > 0).then(|| (total.saturating_sub(available), total))
}

/// A DRM driver's `gpu_busy_percent` (amdgpu, and others that adopt it).
pub(super) fn parse_busy(text: &str) -> Option<u8> {
    text.trim().parse::<u8>().ok().filter(|v| *v <= 100)
}

pub(super) fn cpu_ticks() -> Option<[u32; 4]> {
    parse_cpu(&std::fs::read_to_string("/proc/stat").ok()?)
}

pub(super) fn memory() -> Option<(u64, u64)> {
    parse_memory(&std::fs::read_to_string("/proc/meminfo").ok()?)
}

/// (free, total) bytes of the root file system, free as an ordinary user
/// may use it (`f_bavail`), as the macOS sampler reports its data volume.
// `statvfs`'s counts are `u64` on 64-bit Linux and `u32` on 32-bit: the conversions stay.
#[allow(clippy::useless_conversion)]
pub(super) fn disk() -> Option<(u64, u64)> {
    let mut stats = MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: a NUL-terminated path and a buffer of exactly `statvfs`'s size,
    // which the call fills on success; on failure it is not read.
    let s = unsafe {
        if libc::statvfs(c"/".as_ptr(), stats.as_mut_ptr()) != 0 {
            return None;
        }
        stats.assume_init()
    };
    let unit = u64::from(s.f_frsize);
    if unit == 0 || s.f_blocks == 0 {
        return None;
    }
    Some((
        u64::from(s.f_bavail).saturating_mul(unit),
        u64::from(s.f_blocks).saturating_mul(unit),
    ))
}

pub(super) fn gpu() -> Option<u8> {
    gpu_in(Path::new("/sys/class/drm"))
}

fn gpu_in(drm: &Path) -> Option<u8> {
    std::fs::read_dir(drm)
        .ok()?
        .flatten()
        // `card0`, not its connectors (`card0-HDMI-A-1`) or render nodes.
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("card") && !n.contains('-'))
        })
        .filter_map(|e| {
            parse_busy(&std::fs::read_to_string(e.path().join("device/gpu_busy_percent")).ok()?)
        })
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cpu_line_puts_idle_and_iowait_in_the_idle_slot() {
        let stat = "cpu  100 10 50 800 40 5 5 0 30 0\ncpu0 1 2 3 4\nintr 1\n";
        assert_eq!(parse_cpu(stat), Some([110, 60, 840, 0]));
        assert_eq!(parse_cpu("cpu0 1 2 3 4\n"), None, "only the aggregate line");
        assert_eq!(parse_cpu("cpu  1 2\n"), None, "too short to have an idle");
        // With the shared delta: 10% busy between the two samples.
        let a = parse_cpu("cpu  100 0 0 900 0 0 0 0\n").unwrap();
        let b = parse_cpu("cpu  110 0 0 990 0 0 0 0\n").unwrap();
        assert_eq!(super::super::cpu_percent(a, b), Some(10));
    }

    #[test]
    fn memory_in_use_is_total_less_available() {
        let info = "MemTotal:       16000000 kB\nMemFree:  1000 kB\nMemAvailable:   12000000 kB\n";
        assert_eq!(
            parse_memory(info),
            Some((4_000_000 * 1024, 16_000_000 * 1024))
        );
        assert_eq!(parse_memory("MemFree: 1 kB\n"), None);
    }

    #[test]
    fn the_busiest_gpu_counts_and_none_reporting_is_unavailable() {
        let dir = std::env::temp_dir().join(format!("deckd-drm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (card, busy) in [
            ("card0", Some("12\n")),
            ("card1", Some("47\n")),
            ("card1-DP-1", Some("99\n")),
            ("card2", None),
        ] {
            std::fs::create_dir_all(dir.join(card).join("device")).unwrap();
            if let Some(busy) = busy {
                std::fs::write(dir.join(card).join("device/gpu_busy_percent"), busy).unwrap();
            }
        }
        assert_eq!(gpu_in(&dir), Some(47), "a connector is not a card");
        std::fs::remove_dir_all(dir.join("card0")).unwrap();
        std::fs::remove_dir_all(dir.join("card1")).unwrap();
        assert_eq!(gpu_in(&dir), None);
        assert_eq!(parse_busy("101"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn this_machine_answers() {
        assert!(cpu_ticks().is_some());
        let (used, total) = memory().unwrap();
        assert!(used <= total && total > 0);
        let (free, total) = disk().unwrap();
        assert!(free <= total && total > 0);
    }
}
