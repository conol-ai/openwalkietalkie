//! Windows: hide the window to the tray and bring it back, which gpui cannot
//! do there (its `App::hide` is an empty todo). The window's own handle,
//! through raw-window-handle, and `ShowWindow`.
use gpui::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

const SW_HIDE: i32 = 0;
const SW_SHOW: i32 = 5;

#[link(name = "user32")]
extern "system" {
    fn ShowWindow(window: isize, command: i32) -> i32;
}

/// Show or hide `window`. Hidden, it is off the screen and the taskbar; the
/// tray's Open brings it back.
pub fn set_visible(window: &Window, visible: bool) {
    // The trait's, not gpui's own `Window::window_handle` (its id).
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    if let RawWindowHandle::Win32(win32) = handle.as_raw() {
        // SAFETY: the window's own live handle; ShowWindow only changes its
        // visibility.
        unsafe { ShowWindow(win32.hwnd.get(), if visible { SW_SHOW } else { SW_HIDE }) };
    }
}
