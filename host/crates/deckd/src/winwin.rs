//! Windows' desktop, for the two things the daemon does there on a Mac
//! through AppKit and CoreGraphics (`crate::desktop`): bring a session's app
//! forward, and post a Macropad key's keystroke.
//!
//! An app is named by its executable's file name (`WindowsTerminal.exe`,
//! `Code.exe`): the program that owns a visible top-level window, the kind
//! the taskbar shows. That name is what `hook::app` reports for a session and
//! what [`raise`] looks for, as a bundle identifier is on macOS.
//!
//! Windows lets a process take the foreground only while it may (it has
//! just had input, or the foreground owner allowed it), and a background
//! service never has. A synthetic Alt press and release is the documented
//! way a process earns that right; the window is then restored if it was
//! minimised and brought forward, and the answer is whether it really is in
//! front, never what the calls returned.
//!
//! The calls are declared here rather than through windows-sys features: a
//! feature added to windows-sys rebuilds every crate that depends on it.
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;

type Hwnd = *mut c_void;

const GW_OWNER: u32 = 4;
const GWL_EXSTYLE: i32 = -20;
const WS_EX_TOOLWINDOW: i32 = 0x0000_0080;
const SW_RESTORE: i32 = 9;
const INPUT_KEYBOARD: u32 = 1;
const KEYEVENTF_EXTENDEDKEY: u32 = 0x0001;
const KEYEVENTF_KEYUP: u32 = 0x0002;
const VK_SHIFT: u16 = 0x10;
const VK_CONTROL: u16 = 0x11;
const VK_MENU: u16 = 0x12;

/// `KEYBDINPUT`.
#[repr(C)]
#[derive(Clone, Copy)]
struct KeybdInput {
    vk: u16,
    scan: u16,
    flags: u32,
    time: u32,
    extra: usize,
}

/// `INPUT` for a keyboard event: the type, then the union, whose largest
/// member (`MOUSEINPUT`) is 8 bytes longer than `KEYBDINPUT`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Input {
    kind: u32,
    ki: KeybdInput,
    _mouse_tail: u64,
}

type EnumProc = unsafe extern "system" fn(Hwnd, isize) -> i32;

#[link(name = "user32")]
extern "system" {
    fn EnumWindows(callback: EnumProc, param: isize) -> i32;
    fn GetWindowThreadProcessId(window: Hwnd, pid: *mut u32) -> u32;
    fn IsWindowVisible(window: Hwnd) -> i32;
    fn GetWindow(window: Hwnd, command: u32) -> Hwnd;
    fn GetWindowTextLengthW(window: Hwnd) -> i32;
    fn GetWindowLongW(window: Hwnd, index: i32) -> i32;
    fn IsIconic(window: Hwnd) -> i32;
    fn ShowWindow(window: Hwnd, command: i32) -> i32;
    fn SetForegroundWindow(window: Hwnd) -> i32;
    fn BringWindowToTop(window: Hwnd) -> i32;
    fn GetForegroundWindow() -> Hwnd;
    fn SendInput(count: u32, inputs: *const Input, size: i32) -> u32;
}

unsafe extern "system" fn collect(window: Hwnd, param: isize) -> i32 {
    // SAFETY: `param` is the `Vec` `app_windows` passed, alive for the call.
    let found = &mut *(param as *mut Vec<(Hwnd, u32)>);
    let shown = IsWindowVisible(window) != 0
        && GetWindow(window, GW_OWNER).is_null()
        && GetWindowTextLengthW(window) > 0
        && GetWindowLongW(window, GWL_EXSTYLE) & WS_EX_TOOLWINDOW == 0;
    if shown {
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, &mut pid);
        found.push((window, pid));
    }
    1
}

/// Every app window (visible, unowned, titled, not a tool window) with the
/// process that owns it, front to back.
fn app_windows() -> Vec<(Hwnd, u32)> {
    let mut found: Vec<(Hwnd, u32)> = Vec::new();
    // SAFETY: the callback only writes into `found`, which outlives the call.
    unsafe { EnumWindows(collect, std::ptr::addr_of_mut!(found) as isize) };
    found
}

/// The processes that own an app window.
#[must_use]
pub fn windowed_pids() -> HashSet<u32> {
    app_windows().into_iter().map(|(_, pid)| pid).collect()
}

/// The app windows of the programs whose executable is named `app`,
/// front to back.
fn windows_of(app: &str) -> Vec<Hwnd> {
    let names: HashMap<u32, String> = crate::winproc::snapshot()
        .into_iter()
        .map(|entry| (entry.pid, entry.name))
        .collect();
    app_windows()
        .into_iter()
        .filter(|(_, pid)| names.get(pid).is_some_and(|name| name.eq_ignore_ascii_case(app)))
        .map(|(window, _)| window)
        .collect()
}

/// Whether a program named `app` has a window open.
#[must_use]
pub fn running(app: &str) -> bool {
    !windows_of(app).is_empty()
}

/// The executable name of the program whose window is in front.
fn foreground_app() -> Option<String> {
    // SAFETY: reads only.
    let pid = unsafe {
        let window = GetForegroundWindow();
        if window.is_null() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, &mut pid);
        pid
    };
    crate::winproc::snapshot().into_iter().find(|e| e.pid == pid).map(|e| e.name)
}

/// Bring the front-most window of `app` forward, restoring it if it was
/// minimised. Done when a window of `app` is in front: the switch is
/// asynchronous, so it is given a moment, and any of the program's windows
/// counts, as an app does on macOS.
pub fn raise(app: &str) -> Result<(), String> {
    let in_front = || foreground_app().is_some_and(|name| name.eq_ignore_ascii_case(app));
    if in_front() {
        return Ok(());
    }
    let window = *windows_of(app).first().ok_or("no window")?;
    // SAFETY: a window handle Windows just listed; the calls only ask the
    // window manager to show it.
    unsafe {
        if IsIconic(window) != 0 {
            ShowWindow(window, SW_RESTORE);
        }
        tap(VK_MENU);
        BringWindowToTop(window);
        SetForegroundWindow(window);
    }
    for _ in 0..10 {
        if in_front() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    Err("Windows kept another window in front".into())
}

fn key(vk: u16, up: bool, extended: bool) -> Input {
    let mut flags = 0;
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    Input {
        kind: INPUT_KEYBOARD,
        ki: KeybdInput { vk, scan: 0, flags, time: 0, extra: 0 },
        _mouse_tail: 0,
    }
}

fn send(inputs: &[Input]) -> Result<(), String> {
    // SAFETY: `inputs` is a slice of well-formed keyboard INPUTs.
    let sent = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<Input>() as i32) };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

/// One key pressed and released.
fn tap(vk: u16) {
    let _ = send(&[key(vk, false, false), key(vk, true, false)]);
}

/// The modifiers a keystroke's macOS flags stand for on Windows: Command
/// and Control are both Ctrl (a Mac macro's ⌘C copies here too), Option is
/// Alt and Shift is Shift.
#[must_use]
pub fn modifiers(cmd: bool, ctrl: bool, opt: bool, shift: bool) -> Vec<u16> {
    let mut held = Vec::new();
    if cmd || ctrl {
        held.push(VK_CONTROL);
    }
    if opt {
        held.push(VK_MENU);
    }
    if shift {
        held.push(VK_SHIFT);
    }
    held
}

/// Post `vk` with `held` down around it, as one `SendInput`: the modifiers
/// down, the key down and up, the modifiers up in reverse.
pub fn keystroke(held: &[u16], vk: u16, extended: bool) -> Result<(), String> {
    let mut inputs: Vec<Input> = held.iter().map(|&m| key(m, false, false)).collect();
    inputs.push(key(vk, false, extended));
    inputs.push(key(vk, true, extended));
    inputs.extend(held.iter().rev().map(|&m| key(m, true, false)));
    send(&inputs)
}

/// The Windows virtual key for one of macOS's (`desktop::keys`' Carbon
/// codes), and whether it is an extended key. `None` for a code the deck
/// never sends.
#[must_use]
pub fn virtual_key(mac: u16) -> Option<(u16, bool)> {
    let plain = |vk: u16| Some((vk, false));
    let extended = |vk: u16| Some((vk, true));
    match mac {
        0 => plain(b'A'.into()),
        1 => plain(b'S'.into()),
        2 => plain(b'D'.into()),
        3 => plain(b'F'.into()),
        4 => plain(b'H'.into()),
        5 => plain(b'G'.into()),
        6 => plain(b'Z'.into()),
        7 => plain(b'X'.into()),
        8 => plain(b'C'.into()),
        9 => plain(b'V'.into()),
        11 => plain(b'B'.into()),
        12 => plain(b'Q'.into()),
        13 => plain(b'W'.into()),
        14 => plain(b'E'.into()),
        15 => plain(b'R'.into()),
        16 => plain(b'Y'.into()),
        17 => plain(b'T'.into()),
        18 => plain(b'1'.into()),
        19 => plain(b'2'.into()),
        20 => plain(b'3'.into()),
        21 => plain(b'4'.into()),
        22 => plain(b'6'.into()),
        23 => plain(b'5'.into()),
        24 => plain(0xBB), // equal: VK_OEM_PLUS
        25 => plain(b'9'.into()),
        26 => plain(b'7'.into()),
        27 => plain(0xBD), // minus: VK_OEM_MINUS
        28 => plain(b'8'.into()),
        29 => plain(b'0'.into()),
        30 => plain(0xDD), // rbracket: VK_OEM_6
        31 => plain(b'O'.into()),
        32 => plain(b'U'.into()),
        33 => plain(0xDB), // lbracket: VK_OEM_4
        34 => plain(b'I'.into()),
        35 => plain(b'P'.into()),
        36 => plain(0x0D), // enter
        37 => plain(b'L'.into()),
        38 => plain(b'J'.into()),
        39 => plain(0xDE), // quote: VK_OEM_7
        40 => plain(b'K'.into()),
        41 => plain(0xBA), // semicolon: VK_OEM_1
        42 => plain(0xDC), // backslash: VK_OEM_5
        43 => plain(0xBC), // comma
        44 => plain(0xBF), // slash: VK_OEM_2
        45 => plain(b'N'.into()),
        46 => plain(b'M'.into()),
        47 => plain(0xBE), // period
        48 => plain(0x09), // tab
        49 => plain(0x20), // space
        50 => plain(0xC0), // grave: VK_OEM_3
        51 => plain(0x08), // backspace
        53 => plain(0x1B), // esc
        96 => plain(0x74),  // f5
        97 => plain(0x75),  // f6
        98 => plain(0x76),  // f7
        99 => plain(0x72),  // f3
        100 => plain(0x77), // f8
        101 => plain(0x78), // f9
        103 => plain(0x7A), // f11
        109 => plain(0x79), // f10
        111 => plain(0x7B), // f12
        115 => extended(0x24), // home
        116 => extended(0x21), // pageup
        117 => extended(0x2E), // delete (forward)
        118 => plain(0x73),    // f4
        119 => extended(0x23), // end
        120 => plain(0x71),    // f2
        121 => extended(0x22), // pagedown
        122 => plain(0x70),    // f1
        123 => extended(0x25), // left
        124 => extended(0x27), // right
        125 => extended(0x28), // down
        126 => extended(0x26), // up
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_input_struct_is_windows_size() {
        assert_eq!(std::mem::size_of::<Input>(), 40);
    }

    #[test]
    fn every_key_the_deck_names_has_a_windows_key() {
        for code in (0..=53).chain(96..=126) {
            if matches!(code, 10 | 52 | 102 | 104..=108 | 110 | 112..=114) {
                continue; // codes the deck's names never produce
            }
            assert!(virtual_key(code).is_some(), "mac code {code}");
        }
        assert_eq!(virtual_key(0), Some((0x41, false)));
        assert_eq!(virtual_key(126), Some((0x26, true)));
    }

    #[test]
    fn command_and_control_are_one_ctrl() {
        assert_eq!(modifiers(true, true, false, true), vec![VK_CONTROL, VK_SHIFT]);
        assert_eq!(modifiers(false, false, true, false), vec![VK_MENU]);
    }

    #[test]
    fn this_desktop_has_app_windows_named_by_their_programs() {
        // Whatever is open on the machine running the test: each listed
        // window's program is found again by its name.
        let names: HashMap<u32, String> =
            crate::winproc::snapshot().into_iter().map(|e| (e.pid, e.name)).collect();
        for pid in windowed_pids() {
            if let Some(name) = names.get(&pid) {
                assert!(running(name), "{name}");
            }
        }
    }
}
