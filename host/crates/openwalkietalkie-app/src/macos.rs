//! The one place the window reaches past gpui into AppKit, about how it
//! looks and not about what it does.
//!
//! **The pinned appearance.** [`crate::style::APPEARANCE_ENV`] sets
//! `NSApp.appearance`, so the traffic lights and the window's reported
//! appearance agree with the palette instead of only the palette changing.
//!
//! The window is opaque: there is no blur to re-material and nothing
//! translucent to pin, so nothing else here touches the window.
use objc2::msg_send;
use objc2::runtime::{AnyClass, AnyObject};

/// A harness walk only (`--shots`): keep its window above other apps'
/// ordinary windows and on whichever Space is showing, without taking the
/// keyboard. gpui stops drawing a window macOS reports as not visible, so a
/// walk whose window opened behind the frontmost app (activation is refused
/// while another process holds the front, a system dialog for one) would
/// capture the same empty window for every scene. A product run never calls
/// this.
pub fn float_for_capture() {
    // NSFloatingWindowLevel; canJoinAllSpaces | fullScreenAuxiliary.
    const FLOATING: isize = 3;
    const ALL_SPACES: usize = 1 << 0;
    const FULL_SCREEN_AUXILIARY: usize = 1 << 8;
    let Some(app_class) = AnyClass::get(c"NSApplication") else {
        return;
    };
    // SAFETY: documented NSApplication and NSWindow methods with their
    // documented integer types (NSInteger, NSUInteger); every object is
    // checked for nil before use and nothing is retained past the call.
    unsafe {
        let app: *mut AnyObject = msg_send![app_class, sharedApplication];
        if app.is_null() {
            return;
        }
        let windows: *mut AnyObject = msg_send![app, windows];
        if windows.is_null() {
            return;
        }
        let count: usize = msg_send![windows, count];
        for index in 0..count {
            let window: *mut AnyObject = msg_send![windows, objectAtIndex: index];
            if window.is_null() {
                continue;
            }
            let _: () = msg_send![window, setLevel: FLOATING];
            let behaviour: usize = msg_send![window, collectionBehavior];
            let _: () = msg_send![
                window,
                setCollectionBehavior: behaviour | ALL_SPACES | FULL_SCREEN_AUXILIARY
            ];
            let _: () = msg_send![window, orderFrontRegardless];
        }
    }
}

/// Pin the whole app to Aqua (`dark == false`) or Dark Aqua.
pub fn pin_appearance(dark: bool) {
    let (Some(app_class), Some(appearance_class), Some(string_class)) = (
        AnyClass::get(c"NSApplication"),
        AnyClass::get(c"NSAppearance"),
        AnyClass::get(c"NSString"),
    ) else {
        return;
    };
    let name = if dark {
        c"NSAppearanceNameDarkAqua"
    } else {
        c"NSAppearanceNameAqua"
    };
    // SAFETY: class methods with documented signatures; the string is a
    // NUL-terminated literal and the app object is the shared instance.
    unsafe {
        let name: *mut AnyObject = msg_send![string_class, stringWithUTF8String: name.as_ptr()];
        if name.is_null() {
            return;
        }
        let appearance: *mut AnyObject = msg_send![appearance_class, appearanceNamed: name];
        if appearance.is_null() {
            return;
        }
        let app: *mut AnyObject = msg_send![app_class, sharedApplication];
        let _: () = msg_send![app, setAppearance: appearance];
    }
}
