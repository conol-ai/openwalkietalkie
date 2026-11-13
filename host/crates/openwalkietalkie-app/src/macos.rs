//! The one place the window reaches past gpui into AppKit, about how it
//! looks and not about what it does.
//!
//! **The pinned appearance.** [`crate::style::APPEARANCE_ENV`] sets
//! `NSApp.appearance`, so the traffic lights and the window's reported
//! appearance agree with the palette instead of only the palette changing.
//!
//! **What the Mac asks the look to be** ([`crate::style::look`]): whether
//! Increase contrast is on ([`increase_contrast`]), and whether the app's
//! appearance is a dark one ([`effective_dark`]). The second is AppKit's
//! best match for the appearance among Aqua and Dark Aqua, because gpui
//! 0.2.2 tests the appearance's name against four names and calls any other
//! one light, and the appearances of Increase contrast have names of their
//! own. Both are read, never set.
//!
//! The window is opaque: there is no blur to re-material and nothing
//! translucent to pin, so nothing else here touches the window.
use objc2::msg_send;
use objc2::runtime::{AnyClass, AnyObject};
use objc2_foundation::{NSArray, NSString};

#[link(name = "AppKit", kind = "framework")]
extern "C" {
    /// AppKit's own names for the two appearances the window has a palette
    /// for (`NSAppearance.h`).
    static NSAppearanceNameAqua: &'static NSString;
    static NSAppearanceNameDarkAqua: &'static NSString;
}

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

/// Make the menu at `at` in the menu bar AppKit's windows menu
/// (`NSApp.windowsMenu`), whatever its title: macOS then lists the windows
/// in it and adds Bring All to Front and, from macOS 15, the tiling items,
/// in every language. gpui 0.2.2 does it only for a menu titled exactly
/// "Window" (`crate::app::apply_menu_language`). Nothing happens when the
/// menu bar has no such menu.
pub fn adopt_windows_menu(at: usize) {
    let Some(app_class) = AnyClass::get(c"NSApplication") else {
        return;
    };
    let Ok(at) = isize::try_from(at) else {
        return;
    };
    // SAFETY: documented NSApplication and NSMenu/NSMenuItem methods with
    // their signatures (NSApplication.h, NSMenu.h, NSMenuItem.h); every
    // object is checked for nil and the index against the item count before
    // it is used, and nothing is retained past the call.
    unsafe {
        let app: *mut AnyObject = msg_send![app_class, sharedApplication];
        if app.is_null() {
            return;
        }
        let bar: *mut AnyObject = msg_send![app, mainMenu];
        if bar.is_null() {
            return;
        }
        let count: isize = msg_send![bar, numberOfItems];
        if at >= count {
            return;
        }
        let item: *mut AnyObject = msg_send![bar, itemAtIndex: at];
        if item.is_null() {
            return;
        }
        let menu: *mut AnyObject = msg_send![item, submenu];
        if menu.is_null() {
            return;
        }
        let _: () = msg_send![app, setWindowsMenu: menu];
    }
}

/// The menu bar as AppKit holds it, for a harness walk (`--shots`' menu
/// check): each top menu's title, and the title of the menu AppKit keeps the
/// window list in (`NSApp.windowsMenu`, which [`adopt_windows_menu`] sets).
/// Empty and `None` before there is a menu bar.
#[must_use]
pub fn menu_bar_titles() -> (Vec<String>, Option<String>) {
    let mut titles = Vec::new();
    let Some(app_class) = AnyClass::get(c"NSApplication") else {
        return (titles, None);
    };
    // SAFETY: documented NSApplication, NSMenu and NSMenuItem getters
    // (NSApplication.h, NSMenu.h, NSMenuItem.h); every object is checked for
    // nil and the index stays below the item count; nothing is retained.
    unsafe {
        let app: *mut AnyObject = msg_send![app_class, sharedApplication];
        if app.is_null() {
            return (titles, None);
        }
        let bar: *mut AnyObject = msg_send![app, mainMenu];
        if !bar.is_null() {
            let count: isize = msg_send![bar, numberOfItems];
            for at in 0..count {
                let item: *mut AnyObject = msg_send![bar, itemAtIndex: at];
                if item.is_null() {
                    continue;
                }
                let menu: *mut AnyObject = msg_send![item, submenu];
                let title: *mut NSString = if menu.is_null() {
                    msg_send![item, title]
                } else {
                    msg_send![menu, title]
                };
                if let Some(title) = title.as_ref() {
                    titles.push(title.to_string());
                }
            }
        }
        let windows: *mut AnyObject = msg_send![app, windowsMenu];
        let title: *mut NSString = if windows.is_null() {
            std::ptr::null_mut()
        } else {
            msg_send![windows, title]
        };
        (titles, title.as_ref().map(ToString::to_string))
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

/// Whether the Mac asks for more contrast: Increase contrast, under
/// Accessibility, Display in System Settings. False when AppKit gives no
/// answer.
pub fn increase_contrast() -> bool {
    let Some(workspace_class) = AnyClass::get(c"NSWorkspace") else {
        return false;
    };
    // SAFETY: a documented class method and a documented BOOL getter on its
    // shared instance (NSWorkspace.h, NSAccessibility.h), which is checked
    // for nil; nothing is retained past the call.
    unsafe {
        let workspace: *mut AnyObject = msg_send![workspace_class, sharedWorkspace];
        if workspace.is_null() {
            return false;
        }
        msg_send![workspace, accessibilityDisplayShouldIncreaseContrast]
    }
}

/// Whether the app's appearance, as AppKit draws it now, is a dark one; the
/// pinned one ([`pin_appearance`]) when there is a pin. `None` when AppKit
/// gives no answer.
pub fn effective_dark() -> Option<bool> {
    let app_class = AnyClass::get(c"NSApplication")?;
    // SAFETY: a documented class method and getter (NSApplication.h); the
    // app object is the shared instance, checked for nil, and nothing is
    // retained past the call.
    let appearance: *mut AnyObject = unsafe {
        let app: *mut AnyObject = msg_send![app_class, sharedApplication];
        if app.is_null() {
            return None;
        }
        msg_send![app, effectiveAppearance]
    };
    // SAFETY: the app's own appearance, or nil.
    unsafe { is_dark_appearance(appearance) }
}

/// Whether AppKit's best match for `appearance` among Aqua and Dark Aqua is
/// Dark Aqua: an appearance's name is for matching this way, and a best
/// match is found for any appearance there is, vibrant or high contrast.
/// `None` for nil, or when nothing matches.
///
/// # Safety
/// `appearance` is an `NSAppearance`, or nil.
unsafe fn is_dark_appearance(appearance: *mut AnyObject) -> Option<bool> {
    if appearance.is_null() {
        return None;
    }
    // SAFETY: AppKit's own name constants, and a documented NSAppearance
    // method (macOS 10.14) on the appearance the caller vouches for. The
    // match is one of the names given, or nil, and is not kept.
    unsafe {
        let (aqua, dark_aqua) = (NSAppearanceNameAqua, NSAppearanceNameDarkAqua);
        let names = NSArray::from_slice(&[aqua, dark_aqua]);
        let best: *mut NSString = msg_send![appearance, bestMatchFromAppearancesWithNames: &*names];
        Some(best.as_ref()?.isEqualToString(dark_aqua))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AppKit's appearance named `name`, or nil. `appearanceNamed:` knows
    /// Aqua, Dark Aqua and the two vibrant ones, and no high-contrast name
    /// (those are for matching only), so these are what a test can make.
    fn named(name: &str) -> *mut AnyObject {
        let class = AnyClass::get(c"NSAppearance").expect("AppKit is linked");
        let name = NSString::from_str(name);
        // SAFETY: a documented class method with its documented argument.
        unsafe { msg_send![class, appearanceNamed: &*name] }
    }

    #[test]
    fn an_appearance_is_dark_when_appkits_best_match_for_it_is_dark_aqua() {
        // The constants are the names the pin asks for by value.
        // SAFETY: AppKit's own constants, read.
        let names = unsafe { (NSAppearanceNameAqua, NSAppearanceNameDarkAqua) };
        assert_eq!(names.0.to_string(), "NSAppearanceNameAqua");
        assert_eq!(names.1.to_string(), "NSAppearanceNameDarkAqua");
        for (name, dark) in [
            ("NSAppearanceNameAqua", false),
            ("NSAppearanceNameDarkAqua", true),
            ("NSAppearanceNameVibrantLight", false),
            ("NSAppearanceNameVibrantDark", true),
        ] {
            let appearance = named(name);
            assert!(!appearance.is_null(), "AppKit has no {name}");
            // SAFETY: an NSAppearance.
            let found = unsafe { is_dark_appearance(appearance) };
            assert_eq!(found, Some(dark), "{name}");
        }
        // SAFETY: nil is allowed.
        assert_eq!(unsafe { is_dark_appearance(std::ptr::null_mut()) }, None);
    }

    #[test]
    fn increase_contrast_is_read_as_a_bool_whatever_this_mac_is_set_to() {
        // The getter is there and answers a BOOL: a debug build checks the
        // method's signature on the call, and a wrong one panics.
        let _ = increase_contrast();
    }
}
