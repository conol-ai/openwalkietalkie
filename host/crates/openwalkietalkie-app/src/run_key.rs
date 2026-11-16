//! The per-user Run key, `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`:
//! Windows' login items (see `crate::login_item`). Read, written and cleared
//! value by value; Settings › Apps › Startup's own on/off is read from
//! `Explorer\StartupApproved\Run`. The calls are declared here rather than
//! through a windows-sys dependency the app does not otherwise need.
use std::ffi::c_void;

type Hkey = *mut c_void;
/// `HKEY_CURRENT_USER`, the predefined handle, sign-extended as Windows
/// defines it.
const HKCU: Hkey = 0x8000_0001_u32 as i32 as isize as Hkey;
const REG_SZ: u32 = 1;
const RRF_RT_REG_SZ: u32 = 0x0000_0002;
const RRF_RT_REG_BINARY: u32 = 0x0000_0008;
const ERROR_FILE_NOT_FOUND: i32 = 2;

pub const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

#[link(name = "advapi32")]
extern "system" {
    fn RegGetValueW(
        key: Hkey,
        subkey: *const u16,
        value: *const u16,
        flags: u32,
        kind: *mut u32,
        data: *mut c_void,
        size: *mut u32,
    ) -> i32;
    fn RegSetKeyValueW(
        key: Hkey,
        subkey: *const u16,
        value: *const u16,
        kind: u32,
        data: *const c_void,
        size: u32,
    ) -> i32;
    fn RegDeleteKeyValueW(key: Hkey, subkey: *const u16, value: *const u16) -> i32;
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn read(subkey: &str, name: &str, flags: u32) -> Option<Vec<u8>> {
    let (subkey, name) = (wide(subkey), wide(name));
    let mut size = 0u32;
    // SAFETY: a size query: no buffer.
    let status = unsafe {
        RegGetValueW(HKCU, subkey.as_ptr(), name.as_ptr(), flags, std::ptr::null_mut(), std::ptr::null_mut(), &mut size)
    };
    if status != 0 || size == 0 {
        return None;
    }
    let mut data = vec![0u8; size as usize];
    // SAFETY: `data` holds `size` bytes.
    let status = unsafe {
        RegGetValueW(HKCU, subkey.as_ptr(), name.as_ptr(), flags, std::ptr::null_mut(), data.as_mut_ptr().cast(), &mut size)
    };
    (status == 0).then(|| {
        data.truncate(size as usize);
        data
    })
}

/// A DWORD value under any `HKCU` key (the taskbar's theme, for the tray
/// icon's colour).
pub fn dword(subkey: &str, name: &str) -> Option<u32> {
    const RRF_RT_REG_DWORD: u32 = 0x0000_0010;
    let data = read(subkey, name, RRF_RT_REG_DWORD)?;
    Some(u32::from_le_bytes(data.get(..4)?.try_into().ok()?))
}

/// The command a Run value holds.
pub fn command(name: &str) -> Option<String> {
    let data = read(RUN, name, RRF_RT_REG_SZ)?;
    let units: Vec<u16> = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|unit| *unit != 0)
        .collect();
    Some(String::from_utf16_lossy(&units))
}

/// Whether Settings › Apps › Startup has the value switched on: no
/// record is on; a record whose first byte is odd (3) is off.
pub fn approved(name: &str) -> bool {
    read(APPROVED, name, RRF_RT_REG_BINARY)
        .and_then(|data| data.first().copied())
        .is_none_or(|first| first & 1 == 0)
}

pub fn set(name: &str, command: &str) -> Result<(), String> {
    let (subkey, name) = (wide(RUN), wide(name));
    let data = wide(command);
    // SAFETY: a NUL-terminated REG_SZ of `data.len()` UTF-16 units.
    let status = unsafe {
        RegSetKeyValueW(HKCU, subkey.as_ptr(), name.as_ptr(), REG_SZ, data.as_ptr().cast(), (data.len() * 2) as u32)
    };
    if status == 0 {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(status).to_string())
    }
}

pub fn delete(name: &str) -> Result<(), String> {
    let (subkey, name) = (wide(RUN), wide(name));
    // SAFETY: two NUL-terminated names.
    match unsafe { RegDeleteKeyValueW(HKCU, subkey.as_ptr(), name.as_ptr()) } {
        0 | ERROR_FILE_NOT_FOUND => Ok(()),
        status => Err(std::io::Error::from_raw_os_error(status).to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A value of our own, written, read back and removed; the Run key is
    /// left as it was.
    #[test]
    fn a_value_is_written_read_and_removed() {
        const NAME: &str = "OpenWalkieTalkie Run-key test";
        let _ = delete(NAME);
        assert_eq!(command(NAME), None);
        set(NAME, r#""C:\Program Files\Test\x.exe" --service"#).unwrap();
        assert_eq!(command(NAME).as_deref(), Some(r#""C:\Program Files\Test\x.exe" --service"#));
        assert!(approved(NAME), "no StartupApproved record reads as on");
        delete(NAME).unwrap();
        assert_eq!(command(NAME), None);
        delete(NAME).expect("removing an absent value is not an error");
    }
}