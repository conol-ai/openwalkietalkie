//! Saving one of this process's own windows to a PNG, for looking at the
//! GUI without anyone taking a screenshot (`openwalkietalkie-app --shots`).
//!
//! macOS lets a process capture **its own** windows without the Screen
//! Recording grant, which is what makes this usable from a terminal whose
//! responsible app has no such grant (measured 2026-10-04 on macOS 27: a
//! window of the capturing process comes back with its content; another
//! process's would come back blank). The capture is the window server's
//! composited image, so the Metal-drawn content, the vibrancy and the
//! traffic lights are all in it.
//!
//! `CGWindowListCreateImage` is looked up at run time: newer SDKs mark it
//! obsolete in favour of ScreenCaptureKit (which always asks for the grant),
//! but the symbol is still there, and a link-time reference would tie the
//! build to an SDK that still exports it.
use std::ffi::{c_void, CString};
use std::path::Path;

use gpui::Window;
use objc2::encode::{Encode, Encoding};
use objc2::runtime::AnyObject;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

type CGImageRef = *mut c_void;
type CFTypeRef = *const c_void;

/// `kCGWindowListOptionIncludingWindow`.
const INCLUDING_WINDOW: u32 = 1 << 3;
/// `kCGWindowImageBoundsIgnoreFraming | kCGWindowImageBestResolution`:
/// the window without its shadow, at the backing scale.
const IMAGE_OPTIONS: u32 = (1 << 0) | (1 << 3);
/// `kCFStringEncodingUTF8`.
const UTF8: u32 = 0x0800_0100;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: CFTypeRef,
        buffer: *const u8,
        length: isize,
        is_directory: u8,
    ) -> CFTypeRef;
    fn CFStringCreateWithCString(allocator: CFTypeRef, text: *const i8, encoding: u32)
        -> CFTypeRef;
    fn CFRelease(object: CFTypeRef);
}

#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    fn CGImageDestinationCreateWithURL(
        url: CFTypeRef,
        kind: CFTypeRef,
        count: usize,
        options: CFTypeRef,
    ) -> CFTypeRef;
    fn CGImageDestinationAddImage(destination: CFTypeRef, image: CGImageRef, properties: CFTypeRef);
    fn CGImageDestinationFinalize(destination: CFTypeRef) -> bool;
}

// SAFETY: `CGPoint` is `{double x; double y;}`, as AppKit declares it.
unsafe impl Encode for CGPoint {
    const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
}

/// `NSEventTypeKeyUp`.
const KEY_UP: usize = 11;

/// Let go of `key` in `window`, as the keyboard does after a press: an
/// AppKit key-up handed to gpui's own view (`keyUp:`), which gpui turns
/// into the click a focused control takes when the key is released
/// (`--shots`' keys). Only Return and Space click on release, so any other
/// key is left alone. The event goes to this process's own window and
/// nowhere else.
///
/// # Errors
/// When the window has no AppKit handle or AppKit makes no event.
pub fn release_key(window: &Window, key: &str) -> Result<(), String> {
    let (code, characters) = match key {
        "enter" => (36_u16, "\r"),
        "space" => (49_u16, " "),
        _ => return Ok(()),
    };
    let handle = HasWindowHandle::window_handle(window).map_err(|e| e.to_string())?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return Err("not an AppKit window".into());
    };
    let view = appkit.ns_view.as_ptr().cast::<AnyObject>();
    let characters = objc2_foundation::NSString::from_str(characters);
    // SAFETY: `view` is gpui's live NSView for this window, on the main
    // thread where gpui runs; the event comes from AppKit's own factory and
    // `keyUp:` is the view's own handler for it, as AppKit itself calls it.
    unsafe {
        let ns_window: *mut AnyObject = objc2::msg_send![view, window];
        if ns_window.is_null() {
            return Err("the view has no window yet".into());
        }
        let number: isize = objc2::msg_send![ns_window, windowNumber];
        let event: *mut AnyObject = objc2::msg_send![
            objc2::class!(NSEvent),
            keyEventWithType: KEY_UP,
            location: CGPoint { x: 0., y: 0. },
            modifierFlags: 0_usize,
            timestamp: 0_f64,
            windowNumber: number,
            context: std::ptr::null_mut::<AnyObject>(),
            characters: &*characters,
            charactersIgnoringModifiers: &*characters,
            isARepeat: false,
            keyCode: code
        ];
        if event.is_null() {
            return Err("AppKit made no key-up event".into());
        }
        // Straight to gpui's own view, which is what turns it into a key-up
        // (the window's `sendEvent:` would hand it to its first responder,
        // which need not be that view), and on the run loop's next turn: the
        // caller is inside an update of gpui's app, and gpui drops a key
        // event that arrives while the app is borrowed. A real key arrives
        // from the run loop too. AppKit holds the event until then.
        let () = objc2::msg_send![
            view,
            performSelector: objc2::sel!(keyUp:),
            withObject: event,
            afterDelay: 0_f64
        ];
    }
    Ok(())
}

/// The window server's number for `window`.
fn window_number(window: &Window) -> Result<isize, String> {
    let handle = HasWindowHandle::window_handle(window).map_err(|e| e.to_string())?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return Err("not an AppKit window".into());
    };
    let view = appkit.ns_view.as_ptr().cast::<AnyObject>();
    // SAFETY: `view` is gpui's live NSView for this window; `window` and
    // `windowNumber` are plain getters on the main thread, where gpui runs.
    unsafe {
        let ns_window: *mut AnyObject = objc2::msg_send![view, window];
        if ns_window.is_null() {
            return Err("the view has no window yet".into());
        }
        Ok(objc2::msg_send![ns_window, windowNumber])
    }
}

/// Capture `window` as the window server shows it and write it to `path`
/// as a PNG.
///
/// # Errors
/// When the window has no AppKit handle, the capture comes back empty, or
/// the file cannot be written.
pub fn save_window_png(window: &Window, path: &Path) -> Result<(), String> {
    let number = window_number(window)?;
    // SAFETY: dlopen/dlsym on a system framework; the signature is
    // CGWindowListCreateImage(CGRect, CGWindowListOption, CGWindowID,
    // CGWindowImageOption) -> CGImageRef (CGGeometry.h, CGWindow.h).
    let create: extern "C" fn(CGRect, u32, u32, u32) -> CGImageRef = unsafe {
        let framework =
            CString::new("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
                .expect("no NUL");
        let library = libc::dlopen(framework.as_ptr(), libc::RTLD_NOW);
        if library.is_null() {
            return Err("CoreGraphics did not load".into());
        }
        let name = CString::new("CGWindowListCreateImage").expect("no NUL");
        let symbol = libc::dlsym(library, name.as_ptr());
        if symbol.is_null() {
            return Err("this macOS has no CGWindowListCreateImage".into());
        }
        std::mem::transmute(symbol)
    };
    // CGRectNull: "the bounds of the window", with the including-window
    // option.
    let null = CGRect {
        origin: CGPoint {
            x: f64::INFINITY,
            y: f64::INFINITY,
        },
        size: CGSize {
            width: 0.0,
            height: 0.0,
        },
    };
    let window_id = u32::try_from(number).map_err(|_| format!("odd window number {number}"))?;
    let image = create(null, INCLUDING_WINDOW, window_id, IMAGE_OPTIONS);
    if image.is_null() {
        return Err("the capture came back empty".into());
    }
    let bytes = path.as_os_str().as_encoded_bytes();
    let kind = CString::new("public.png").expect("no NUL");
    // SAFETY: every object below is created here and released here; the
    // calls are ImageIO's documented create/add/finalize sequence.
    let written = unsafe {
        let url = CFURLCreateFromFileSystemRepresentation(
            std::ptr::null(),
            bytes.as_ptr(),
            bytes.len() as isize,
            0,
        );
        let uti = CFStringCreateWithCString(std::ptr::null(), kind.as_ptr(), UTF8);
        let destination = CGImageDestinationCreateWithURL(url, uti, 1, std::ptr::null());
        let ok = if destination.is_null() {
            false
        } else {
            CGImageDestinationAddImage(destination, image, std::ptr::null());
            let ok = CGImageDestinationFinalize(destination);
            CFRelease(destination);
            ok
        };
        CFRelease(uti);
        CFRelease(url);
        CFRelease(image.cast_const());
        ok
    };
    if written {
        Ok(())
    } else {
        Err(format!("could not write {}", path.display()))
    }
}
