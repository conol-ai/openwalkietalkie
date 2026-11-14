//! Windows counters for the System face, matching the macOS readings:
//! CPU from `GetSystemTimes` deltas, memory in use as total minus available
//! (`GlobalMemoryStatusEx`, Task Manager's "In use"), the system drive's
//! space available to this user (`GetDiskFreeSpaceExW`), and GPU load from
//! the `GPU Engine` performance counters, the way Task Manager adds them up.
//! No subprocesses and no admin rights. A reading Windows does not give is
//! unavailable, never an invented 0%.
//!
//! The few calls are declared here rather than through windows-sys features:
//! a feature added to windows-sys rebuilds every crate that depends on it.
use std::collections::HashMap;
use std::ffi::c_void;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FileTime {
    low: u32,
    high: u32,
}

impl FileTime {
    /// Milliseconds: two seconds on every core stays far inside `u32`, and
    /// `cpu_percent` takes wrapping deltas.
    fn millis(self) -> u32 {
        (((u64::from(self.high) << 32) | u64::from(self.low)) / 10_000) as u32
    }
}

#[repr(C)]
struct MemoryStatusEx {
    length: u32,
    memory_load: u32,
    total_phys: u64,
    avail_phys: u64,
    total_page_file: u64,
    avail_page_file: u64,
    total_virtual: u64,
    avail_virtual: u64,
    avail_extended_virtual: u64,
}

/// `PDH_FMT_COUNTERVALUE` read as a double: the status, then the union,
/// 8-aligned.
#[repr(C)]
struct FmtValue {
    status: u32,
    value: f64,
}

/// `PDH_FMT_COUNTERVALUE_ITEM_W`.
#[repr(C)]
struct FmtItem {
    name: *const u16,
    value: FmtValue,
}

type Handle = *mut c_void;

#[link(name = "kernel32")]
extern "system" {
    fn GetSystemTimes(idle: *mut FileTime, kernel: *mut FileTime, user: *mut FileTime) -> i32;
    fn GlobalMemoryStatusEx(status: *mut MemoryStatusEx) -> i32;
    fn GetDiskFreeSpaceExW(
        directory: *const u16,
        free_to_caller: *mut u64,
        total: *mut u64,
        total_free: *mut u64,
    ) -> i32;
}

#[link(name = "pdh")]
extern "system" {
    fn PdhOpenQueryW(source: *const u16, user: usize, query: *mut Handle) -> u32;
    fn PdhAddEnglishCounterW(query: Handle, path: *const u16, user: usize, counter: *mut Handle)
        -> u32;
    fn PdhCollectQueryData(query: Handle) -> u32;
    fn PdhGetFormattedCounterArrayW(
        counter: Handle,
        format: u32,
        size: *mut u32,
        count: *mut u32,
        items: *mut FmtItem,
    ) -> u32;
    fn PdhCloseQuery(query: Handle) -> u32;
}

const PDH_FMT_DOUBLE: u32 = 0x0000_0200;
const PDH_MORE_DATA: u32 = 0x8000_07D2;
const PDH_CSTATUS_VALID_DATA: u32 = 0;
const PDH_CSTATUS_NEW_DATA: u32 = 1;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The ticks `cpu_percent` takes, in its Mach order: user, system, idle,
/// nice. Windows' kernel time includes the idle time, so system is kernel
/// less idle, and there is no nice.
pub(super) fn cpu_ticks() -> Option<[u32; 4]> {
    let (mut idle, mut kernel, mut user) = Default::default();
    // SAFETY: three out-parameters we own.
    if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
        return None;
    }
    let (idle, kernel, user) = (idle.millis(), kernel.millis(), user.millis());
    Some([user, kernel.wrapping_sub(idle), idle, 0])
}

/// (in use, total) bytes of physical memory.
pub(super) fn memory() -> Option<(u64, u64)> {
    let mut status = MemoryStatusEx {
        length: std::mem::size_of::<MemoryStatusEx>() as u32,
        memory_load: 0,
        total_phys: 0,
        avail_phys: 0,
        total_page_file: 0,
        avail_page_file: 0,
        total_virtual: 0,
        avail_virtual: 0,
        avail_extended_virtual: 0,
    };
    // SAFETY: `length` is set, as the call requires.
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 || status.total_phys == 0 {
        return None;
    }
    Some((status.total_phys.saturating_sub(status.avail_phys), status.total_phys))
}

/// (free to this user, total) bytes of the system drive, the volume Windows
/// and the user's profile live on.
pub(super) fn disk() -> Option<(u64, u64)> {
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    let root = wide(&format!("{drive}\\"));
    let (mut free, mut total, mut total_free) = (0u64, 0u64, 0u64);
    // SAFETY: a NUL-terminated path and three out-parameters we own.
    if unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut free, &mut total, &mut total_free) } == 0
        || total == 0
    {
        return None;
    }
    Some((free, total))
}

/// The open `GPU Engine` counter query. Its utilization is a rate, so the
/// first collection after opening yields nothing and every later one the
/// load since the one before.
pub(super) struct Gpu {
    query: Handle,
    counter: Handle,
}

// SAFETY: the query is used by the one sampler thread that owns it, and
// closed once, on drop.
unsafe impl Send for Gpu {}

impl Drop for Gpu {
    fn drop(&mut self) {
        // SAFETY: opened by `open`, closed once.
        unsafe { PdhCloseQuery(self.query) };
    }
}

impl Gpu {
    /// `None` where the counter set does not exist (no WDDM 2 driver, as on
    /// a virtual machine's basic display adapter).
    pub(super) fn open() -> Option<Gpu> {
        let mut query: Handle = std::ptr::null_mut();
        // SAFETY: out-parameter we own; a local real-time query.
        if unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut query) } != 0 {
            return None;
        }
        let mut counter: Handle = std::ptr::null_mut();
        let path = wide(r"\GPU Engine(*)\Utilization Percentage");
        // SAFETY: a NUL-terminated path; the query is ours.
        if unsafe { PdhAddEnglishCounterW(query, path.as_ptr(), 0, &mut counter) } != 0 {
            // SAFETY: opened above.
            unsafe { PdhCloseQuery(query) };
            return None;
        }
        let gpu = Gpu { query, counter };
        // The rate's first reference point.
        // SAFETY: our query.
        unsafe { PdhCollectQueryData(gpu.query) };
        Some(gpu)
    }

    /// The busiest engine's load since the last call, 0..=100: each engine
    /// is one adapter's one engine type (3D, copy, video decode, …), its
    /// load the sum over the processes using it, as Task Manager adds them.
    pub(super) fn load(&mut self) -> Option<u8> {
        // SAFETY: our query.
        if unsafe { PdhCollectQueryData(self.query) } != 0 {
            return None;
        }
        let (mut size, mut count) = (0u32, 0u32);
        // SAFETY: a size query: no buffer, zero size.
        let status = unsafe {
            PdhGetFormattedCounterArrayW(
                self.counter,
                PDH_FMT_DOUBLE,
                &mut size,
                &mut count,
                std::ptr::null_mut(),
            )
        };
        if status != PDH_MORE_DATA || size == 0 {
            return None;
        }
        // u64 words keep the items 8-aligned; the names follow them.
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        let items = buffer.as_mut_ptr().cast::<FmtItem>();
        // SAFETY: `buffer` holds `size` bytes, as the first call asked.
        if unsafe {
            PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut size, &mut count, items)
        } != 0
        {
            return None;
        }
        let mut engines: HashMap<String, f64> = HashMap::new();
        for index in 0..count as usize {
            // SAFETY: `count` items were written at the start of `buffer`,
            // each name a NUL-terminated string inside it.
            let (name, value) = unsafe {
                let item = &*items.add(index);
                if !matches!(item.value.status, PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA) {
                    continue;
                }
                (wide_to_string(item.name), item.value.value)
            };
            if let Some(engine) = engine_of(&name) {
                *engines.entry(engine.to_string()).or_default() += value.max(0.0);
            }
        }
        busiest(engines.into_values())
    }
}

/// # Safety
/// `ptr` is null or points at a NUL-terminated UTF-16 string.
unsafe fn wide_to_string(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
}

/// The engine an instance belongs to: its name without the process,
/// `pid_1234_luid_0x0_0xD1A2_phys_0_eng_3_engtype_3D` →
/// `luid_0x0_0xD1A2_phys_0_eng_3_engtype_3D`. `None` for any other shape.
fn engine_of(instance: &str) -> Option<&str> {
    let rest = instance.strip_prefix("pid_")?;
    let at = rest.find("_luid_")?;
    Some(&rest[at + 1..])
}

/// The highest engine load, rounded and held to 0..=100; `None` when no
/// engine reported one.
fn busiest(loads: impl Iterator<Item = f64>) -> Option<u8> {
    loads
        .filter(|load| load.is_finite())
        .fold(None, |most: Option<f64>, load| Some(most.map_or(load, |m| m.max(load))))
        .map(|load| load.round().clamp(0.0, 100.0) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_engine_is_its_instance_without_the_process() {
        assert_eq!(
            engine_of("pid_1234_luid_0x00000000_0x0000D1A2_phys_0_eng_3_engtype_3D"),
            Some("luid_0x00000000_0x0000D1A2_phys_0_eng_3_engtype_3D")
        );
        assert_eq!(engine_of("_Total"), None);
        assert_eq!(engine_of("pid_12_nothing"), None);
    }

    #[test]
    fn the_busiest_engine_wins_and_is_held_to_a_percentage() {
        assert_eq!(busiest([12.4, 61.6, 3.0].into_iter()), Some(62));
        assert_eq!(busiest([140.0].into_iter()), Some(100));
        assert_eq!(busiest(std::iter::empty()), None);
        assert_eq!(busiest([f64::NAN].into_iter()), None);
    }

    /// The live counters, on the machine the test runs on: each is either
    /// unavailable or a reading in range, and two CPU samples a moment
    /// apart give a percentage.
    #[test]
    fn the_live_readings_are_in_range() {
        let first = cpu_ticks().expect("GetSystemTimes answers");
        std::thread::sleep(std::time::Duration::from_millis(200));
        let second = cpu_ticks().expect("GetSystemTimes answers");
        if let Some(cpu) = super::super::cpu_percent(first, second) {
            assert!(cpu <= 100);
        }
        let (used, total) = memory().expect("GlobalMemoryStatusEx answers");
        assert!(used <= total && total > 0);
        let (free, total) = disk().expect("the system drive has a size");
        assert!(free <= total && total > 0);
        if let Some(mut gpu) = Gpu::open() {
            std::thread::sleep(std::time::Duration::from_millis(200));
            assert!(gpu.load().is_none_or(|load| load <= 100));
        }
    }
}
