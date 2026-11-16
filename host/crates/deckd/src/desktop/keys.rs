//! Keystrokes for Macropad keys (`DesktopJob::Keystroke`): a combo such as
//! `cmd+shift+4` is posted to the desktop as a key-down / key-up pair with
//! its modifier flags, through CoreGraphics on macOS (the way openmicrokbd
//! types for its app). The deck itself is never a keyboard: its USB block
//! has a single IN endpoint, so a keyboard collection would have to share
//! the vendor interface, and macOS then gates the whole deck behind Input
//! Monitoring for every program that opens it.
//!
//! Posting needs the Accessibility permission for this process; a refusal
//! is logged with the setting to change, `AXIsProcessTrusted` is asked
//! again on every press so granting it takes effect at once, and the
//! desktop side has macOS put its own prompt up once per run ([`trusted`])
//! so the operator is shown the row to switch on.
//!
//! The names are the deck's (`deck_proto::KEYSTROKE_KEYS`); `parse` turns
//! them into the Mac's virtual key codes (Carbon's `kVK_*`, ANSI layout)
//! and `CGEventFlags`, and is pure, so it is tested on every platform.
use std::fmt;

/// `CGEventFlags` masks.
pub const FLAG_SHIFT: u64 = 1 << 17;
pub const FLAG_CTRL: u64 = 1 << 18;
pub const FLAG_OPT: u64 = 1 << 19;
pub const FLAG_CMD: u64 = 1 << 20;

/// A parsed combo: the modifier flags and the virtual key code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stroke {
    pub flags: u64,
    pub keycode: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    /// Not a combo this daemon can post.
    Unknown(String),
    /// macOS refused: Accessibility is not granted to this binary.
    NotTrusted,
    /// No desktop on this platform.
    Unsupported,
    /// CoreGraphics gave no event.
    Event,
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyError::Unknown(what) => write!(f, "unknown keystroke {what:?}"),
            KeyError::NotTrusted => f.write_str(
                "Accessibility is not granted: System Settings > Privacy & Security > Accessibility, add deckd",
            ),
            KeyError::Unsupported => f.write_str("keystrokes are posted on macOS and Windows only"),
            KeyError::Event => f.write_str("the system did not take the keystroke"),
        }
    }
}

impl std::error::Error for KeyError {}

/// Carbon's virtual key codes for the ANSI layout.
fn keycode(name: &str) -> Option<u16> {
    Some(match name {
        "a" => 0,
        "s" => 1,
        "d" => 2,
        "f" => 3,
        "h" => 4,
        "g" => 5,
        "z" => 6,
        "x" => 7,
        "c" => 8,
        "v" => 9,
        "b" => 11,
        "q" => 12,
        "w" => 13,
        "e" => 14,
        "r" => 15,
        "y" => 16,
        "t" => 17,
        "1" => 18,
        "2" => 19,
        "3" => 20,
        "4" => 21,
        "6" => 22,
        "5" => 23,
        "equal" => 24,
        "9" => 25,
        "7" => 26,
        "minus" => 27,
        "8" => 28,
        "0" => 29,
        "rbracket" => 30,
        "o" => 31,
        "u" => 32,
        "lbracket" => 33,
        "i" => 34,
        "p" => 35,
        "enter" => 36,
        "l" => 37,
        "j" => 38,
        "quote" => 39,
        "k" => 40,
        "semicolon" => 41,
        "backslash" => 42,
        "comma" => 43,
        "slash" => 44,
        "n" => 45,
        "m" => 46,
        "period" => 47,
        "tab" => 48,
        "space" => 49,
        "grave" => 50,
        "backspace" => 51,
        "esc" => 53,
        "f5" => 96,
        "f6" => 97,
        "f7" => 98,
        "f3" => 99,
        "f8" => 100,
        "f9" => 101,
        "f11" => 103,
        "f10" => 109,
        "f12" => 111,
        "home" => 115,
        "pageup" => 116,
        "delete" => 117,
        "f4" => 118,
        "end" => 119,
        "f2" => 120,
        "pagedown" => 121,
        "f1" => 122,
        "left" => 123,
        "right" => 124,
        "down" => 125,
        "up" => 126,
        _ => return None,
    })
}

/// `cmd+shift+4` → flags and key code. Modifiers in any order; `alt` is
/// `opt`; exactly one key.
pub fn parse(combo: &str) -> Result<Stroke, KeyError> {
    let mut flags = 0;
    let mut code = None;
    for part in combo.split('+') {
        match part {
            "ctrl" => flags |= FLAG_CTRL,
            "opt" | "alt" => flags |= FLAG_OPT,
            "shift" => flags |= FLAG_SHIFT,
            "cmd" => flags |= FLAG_CMD,
            name => {
                if code.is_some() {
                    return Err(KeyError::Unknown(combo.into()));
                }
                code = Some(keycode(name).ok_or_else(|| KeyError::Unknown(combo.into()))?);
            }
        }
    }
    Ok(Stroke {
        flags,
        keycode: code.ok_or_else(|| KeyError::Unknown(combo.into()))?,
    })
}

/// Post `combo` to the desktop.
pub fn press(combo: &str) -> Result<(), KeyError> {
    let stroke = parse(combo)?;
    post(stroke)
}

/// Whether macOS trusts this process to post events (the Accessibility
/// grant). With `prompt`, macOS also puts its own dialog up and lists the
/// process in System Settings > Privacy & Security > Accessibility, which
/// is the only way an operator ever finds the right row for a bare binary;
/// the dialog names the *responsible* process, so for a daemon started
/// from a terminal that is the terminal's app. `None` off macOS.
#[cfg(target_os = "macos")]
pub fn trusted(prompt: bool) -> Option<bool> {
    use std::ffi::c_void;
    use std::ptr;
    /// `CFDictionaryKeyCallBacks` (CFDictionary.h): six words.
    #[repr(C)]
    struct KeyCallBacks {
        version: isize,
        retain: *const c_void,
        release: *const c_void,
        copy_description: *const c_void,
        equal: *const c_void,
        hash: *const c_void,
    }
    /// `CFDictionaryValueCallBacks`: five words.
    #[repr(C)]
    struct ValueCallBacks {
        version: isize,
        retain: *const c_void,
        release: *const c_void,
        copy_description: *const c_void,
        equal: *const c_void,
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFTypeDictionaryKeyCallBacks: KeyCallBacks;
        static kCFTypeDictionaryValueCallBacks: ValueCallBacks;
        static kCFBooleanTrue: *const c_void;
        fn CFDictionaryCreate(
            allocator: *const c_void,
            keys: *const *const c_void,
            values: *const *const c_void,
            num_values: isize,
            key_callbacks: *const KeyCallBacks,
            value_callbacks: *const ValueCallBacks,
        ) -> *const c_void;
        fn CFRelease(cf: *const c_void);
    }
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        static kAXTrustedCheckOptionPrompt: *const c_void;
        fn AXIsProcessTrusted() -> u8;
        fn AXIsProcessTrustedWithOptions(options: *const c_void) -> u8;
    }
    // SAFETY: plain CoreFoundation and HIServices calls on constants the
    // frameworks export; the one object created is released here.
    unsafe {
        if !prompt {
            return Some(AXIsProcessTrusted() != 0);
        }
        let keys = [kAXTrustedCheckOptionPrompt];
        let values = [kCFBooleanTrue];
        let options = CFDictionaryCreate(
            ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            ptr::addr_of!(kCFTypeDictionaryKeyCallBacks),
            ptr::addr_of!(kCFTypeDictionaryValueCallBacks),
        );
        let trusted = AXIsProcessTrustedWithOptions(options) != 0;
        if !options.is_null() {
            CFRelease(options);
        }
        Some(trusted)
    }
}

/// Windows asks for no grant to post keys to the user's own desktop.
#[cfg(windows)]
pub fn trusted(_prompt: bool) -> Option<bool> {
    Some(true)
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn trusted(_prompt: bool) -> Option<bool> {
    None
}

#[cfg(target_os = "macos")]
fn post(stroke: Stroke) -> Result<(), KeyError> {
    use std::ffi::c_void;
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceCreate(state: i32) -> *mut c_void;
        fn CGEventCreateKeyboardEvent(source: *mut c_void, key: u16, down: bool) -> *mut c_void;
        fn CGEventSetFlags(event: *mut c_void, flags: u64);
        fn CGEventPost(tap: u32, event: *mut c_void);
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: *const c_void);
    }
    const HID_SYSTEM_STATE: i32 = 1;
    const HID_EVENT_TAP: u32 = 0;
    if trusted(false) != Some(true) {
        return Err(KeyError::NotTrusted);
    }
    // SAFETY: plain CoreGraphics calls; every object created here is
    // released here, and a null event is reported, never used.
    unsafe {
        let source = CGEventSourceCreate(HID_SYSTEM_STATE);
        let mut result = Ok(());
        for down in [true, false] {
            let event = CGEventCreateKeyboardEvent(source, stroke.keycode, down);
            if event.is_null() {
                result = Err(KeyError::Event);
                break;
            }
            CGEventSetFlags(event, stroke.flags);
            CGEventPost(HID_EVENT_TAP, event);
            CFRelease(event);
        }
        if !source.is_null() {
            CFRelease(source);
        }
        result
    }
}

/// Windows: one `SendInput` with the modifiers around the key
/// (`crate::winwin`). Command and Control are both Ctrl there, so a macro
/// made on a Mac does the same thing on Windows.
#[cfg(windows)]
fn post(stroke: Stroke) -> Result<(), KeyError> {
    let (vk, extended) =
        crate::winwin::virtual_key(stroke.keycode).ok_or_else(|| KeyError::Unknown(format!("{stroke:?}")))?;
    let held = crate::winwin::modifiers(
        stroke.flags & FLAG_CMD != 0,
        stroke.flags & FLAG_CTRL != 0,
        stroke.flags & FLAG_OPT != 0,
        stroke.flags & FLAG_SHIFT != 0,
    );
    crate::winwin::keystroke(&held, vk, extended).map_err(|_| KeyError::Event)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn post(_stroke: Stroke) -> Result<(), KeyError> {
    Err(KeyError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_deck_key_name_has_a_key_code_and_combos_parse() {
        for name in deck_proto::KEYSTROKE_KEYS {
            assert!(keycode(name).is_some(), "{name}");
            assert!(parse(&format!("cmd+shift+{name}")).is_ok(), "{name}");
        }
        assert_eq!(
            parse("shift+cmd+4"),
            Ok(Stroke { flags: FLAG_SHIFT | FLAG_CMD, keycode: 21 })
        );
        assert_eq!(parse("ctrl+alt+delete").map(|s| s.flags), Ok(FLAG_CTRL | FLAG_OPT));
        assert_eq!(parse("c").map(|s| s.keycode), Ok(8));
        for bad in ["", "cmd", "cmd+c+v", "super+c", "cmd+ptt"] {
            assert!(matches!(parse(bad), Err(KeyError::Unknown(_))), "{bad}");
        }
        // The two tables agree on what counts as a key.
        for name in deck_proto::KEYSTROKE_KEYS {
            assert!(deck_proto::keystroke_valid(&format!("opt+{name}")));
        }
    }
}
