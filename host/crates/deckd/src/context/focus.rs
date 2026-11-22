//! The focused window, as two strings: the application's name and its
//! window title.
//!
//! ## PRIVACY: THIS IS ALL THAT IS EVER COLLECTED. DO NOT WIDEN IT.
//! Two strings, and nothing else: no screenshot, no pixels, no window
//! contents, no accessibility-tree text beyond the window's own title, no
//! process list, no shell history. The scene model sees these two strings
//! and the edit model sees neither (only the transcript and a scene label).
//! A change that reads anything more from the screen breaks a promise made
//! to the owner ("screenshots never leave the machine"); it needs the
//! owner's decision, not a code review.
//!
//! Every reader is bounded in time and fails quietly: no focus is the
//! rules' least-change scene, and dictation goes on.
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::time::Duration;

/// The two strings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Focus {
    /// The application: Hyprland's window class (`foot`, `Slack`), the
    /// Mac's localised app name, the Windows executable's file stem.
    pub app: String,
    /// The focused window's title, `""` when it cannot be read.
    pub title: String,
}

/// The focused window now, or `None`.
#[must_use]
pub fn read() -> Option<Focus> {
    platform::read().filter(|focus| !focus.app.is_empty() || !focus.title.is_empty())
}

/// A program's output within `timeout`, or `None`. `env` is added to its
/// environment (never to this process's).
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn output(
    program: &str,
    args: &[&str],
    env: &[(&str, &str)],
    timeout: Duration,
) -> Option<Vec<u8>> {
    use std::io::Read as _;
    use std::process::{Command, Stdio};
    let mut child = Command::new(program)
        .args(args)
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let reader = child.stdout.take().map(|stdout| {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            let _ = stdout.take(64 * 1024).read_to_end(&mut out);
            out
        })
    });
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(2)),
        }
    }
    reader.and_then(|r| r.join().ok())
}

/// Linux: Hyprland, through `hyprctl activewindow -j`. Verified on Omarchy.
#[cfg(target_os = "linux")]
mod platform {
    use super::{output, Focus};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    const TIMEOUT: Duration = Duration::from_millis(400);

    fn runtime_dir() -> PathBuf {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            // SAFETY: getuid cannot fail.
            .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })))
    }

    fn alive(hypr: &Path, signature: &str) -> bool {
        std::os::unix::net::UnixStream::connect(hypr.join(signature).join(".socket.sock")).is_ok()
    }

    /// The running Hyprland's instance signature. The PoC's quirk
    /// (`omarchy_hypr.ensure_instance`): a service started by systemd has
    /// no `HYPRLAND_INSTANCE_SIGNATURE`, and an inherited one goes stale when
    /// the compositor restarts, so the newest instance whose command socket
    /// answers wins. It is given to `hyprctl` alone, never set on deckd.
    pub fn signature() -> Option<String> {
        let hypr = runtime_dir().join("hypr");
        if let Some(current) = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
            .ok()
            .filter(|s| !s.is_empty())
        {
            if alive(&hypr, &current) {
                return Some(current);
            }
        }
        let mut found: Vec<(std::time::SystemTime, String)> = std::fs::read_dir(&hypr)
            .ok()?
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| {
                let modified = entry.metadata().ok()?.modified().ok()?;
                Some((modified, entry.file_name().to_string_lossy().into_owned()))
            })
            .collect();
        found.sort_by_key(|a| std::cmp::Reverse(a.0));
        found
            .into_iter()
            .map(|(_, name)| name)
            .find(|name| alive(&hypr, name))
    }

    pub fn read() -> Option<Focus> {
        let signature = signature()?;
        let runtime = runtime_dir();
        let out = output(
            "hyprctl",
            &["activewindow", "-j"],
            &[
                ("HYPRLAND_INSTANCE_SIGNATURE", signature.as_str()),
                ("XDG_RUNTIME_DIR", &runtime.to_string_lossy()),
            ],
            TIMEOUT,
        )?;
        parse(&out)
    }

    /// The two fields of `activewindow -j`, and only them.
    pub fn parse(json: &[u8]) -> Option<Focus> {
        let value: serde_json::Value = serde_json::from_slice(json).ok()?;
        let text = |key: &str| {
            value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let app = Some(text("class"))
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| text("initialClass"));
        Some(Focus {
            app,
            title: text("title"),
        })
    }
}

/// macOS. **UNVERIFIED: written without a Mac to build or run it on.**
/// The app's name comes from `lsappinfo` (part of macOS, needs no
/// permission). The window title needs the **Accessibility** permission:
/// it is read only when this process already holds it
/// (`AXIsProcessTrusted`, which never prompts) — deckd asks for that grant
/// for the Macropad's keystrokes already (`desktop::keys`), and this code
/// never puts a prompt of its own up. Without it the title is `""` and the
/// rules decide from the app's name.
#[cfg(target_os = "macos")]
mod platform {
    use super::{output, Duration, Focus};

    const TIMEOUT: Duration = Duration::from_millis(400);

    pub fn read() -> Option<Focus> {
        let front = output("/usr/bin/lsappinfo", &["front"], &[], TIMEOUT)?;
        let asn = String::from_utf8_lossy(&front).trim().to_string();
        if asn.is_empty() {
            return None;
        }
        let info = output(
            "/usr/bin/lsappinfo",
            &["info", "-only", "name", &asn],
            &[],
            TIMEOUT,
        )?;
        let app = name_of(&String::from_utf8_lossy(&info))?;
        let pid = output(
            "/usr/bin/lsappinfo",
            &["info", "-only", "pid", &asn],
            &[],
            TIMEOUT,
        )
        .and_then(|out| pid_of(&String::from_utf8_lossy(&out)));
        let title = pid.and_then(ax::window_title).unwrap_or_default();
        Some(Focus { app, title })
    }

    /// `"LSDisplayName"="Ghostty"` → `Ghostty`.
    fn name_of(text: &str) -> Option<String> {
        let value = text.split_once('=')?.1.trim().trim_matches('"').to_string();
        (!value.is_empty()).then_some(value)
    }

    /// `"pid"=4242` → 4242.
    fn pid_of(text: &str) -> Option<i32> {
        text.split_once('=')?.1.trim().parse().ok()
    }

    mod ax {
        use std::ffi::c_void;
        type CFTypeRef = *const c_void;

        #[link(name = "CoreFoundation", kind = "framework")]
        extern "C" {
            fn CFStringCreateWithCString(
                alloc: CFTypeRef,
                text: *const i8,
                encoding: u32,
            ) -> CFTypeRef;
            fn CFStringGetCString(
                string: CFTypeRef,
                buffer: *mut i8,
                size: isize,
                encoding: u32,
            ) -> u8;
            fn CFGetTypeID(cf: CFTypeRef) -> usize;
            fn CFStringGetTypeID() -> usize;
            fn CFRelease(cf: CFTypeRef);
        }
        #[link(name = "ApplicationServices", kind = "framework")]
        extern "C" {
            fn AXIsProcessTrusted() -> u8;
            fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
            fn AXUIElementCopyAttributeValue(
                element: CFTypeRef,
                attribute: CFTypeRef,
                value: *mut CFTypeRef,
            ) -> i32;
        }
        const UTF8: u32 = 0x0800_0100;

        /// One attribute of `element`, owned by the caller (`None`: not
        /// there, or refused).
        unsafe fn attribute(element: CFTypeRef, name: &std::ffi::CStr) -> Option<CFTypeRef> {
            let key = CFStringCreateWithCString(std::ptr::null(), name.as_ptr(), UTF8);
            if key.is_null() {
                return None;
            }
            let mut value: CFTypeRef = std::ptr::null();
            let error = AXUIElementCopyAttributeValue(element, key, &mut value);
            CFRelease(key);
            (error == 0 && !value.is_null()).then_some(value)
        }

        /// The title of `pid`'s focused window, only with the grant.
        pub fn window_title(pid: i32) -> Option<String> {
            // SAFETY: CoreFoundation/HIServices calls; every object this
            // creates or copies is released before returning.
            unsafe {
                if AXIsProcessTrusted() == 0 {
                    return None;
                }
                let app = AXUIElementCreateApplication(pid);
                if app.is_null() {
                    return None;
                }
                let window = attribute(app, c"AXFocusedWindow");
                CFRelease(app);
                let window = window?;
                let title = attribute(window, c"AXTitle");
                CFRelease(window);
                let title = title?;
                let mut buffer = vec![0i8; 1024];
                let ok = CFGetTypeID(title) == CFStringGetTypeID()
                    && CFStringGetCString(title, buffer.as_mut_ptr(), buffer.len() as isize, UTF8)
                        != 0;
                CFRelease(title);
                if !ok {
                    return None;
                }
                let text = std::ffi::CStr::from_ptr(buffer.as_ptr())
                    .to_string_lossy()
                    .into_owned();
                Some(text)
            }
        }
    }
}

/// Windows. **UNVERIFIED: written without a Windows machine to build or
/// run it on.** `GetForegroundWindow` and `GetWindowTextW` (no permission
/// needed for the user's own desktop), and the owning process's executable
/// stem as the app's name (`winproc::exe_path`).
#[cfg(windows)]
mod platform {
    use super::Focus;
    type Hwnd = *mut std::ffi::c_void;

    #[link(name = "user32")]
    extern "system" {
        fn GetForegroundWindow() -> Hwnd;
        fn GetWindowTextW(window: Hwnd, text: *mut u16, max: i32) -> i32;
        fn GetWindowThreadProcessId(window: Hwnd, pid: *mut u32) -> u32;
    }

    pub fn read() -> Option<Focus> {
        // SAFETY: plain user32 calls on the foreground window handle, which
        // may be null (checked) and is never dereferenced here.
        unsafe {
            let window = GetForegroundWindow();
            if window.is_null() {
                return None;
            }
            let mut buf = [0u16; 512];
            let n = GetWindowTextW(window, buf.as_mut_ptr(), buf.len() as i32).max(0) as usize;
            let title = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
            let mut pid = 0u32;
            GetWindowThreadProcessId(window, &mut pid);
            let app = crate::winproc::exe_path(pid)
                .and_then(|path| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
                .unwrap_or_default();
            Some(Focus { app, title })
        }
    }
}

/// Anywhere else: no focus.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod platform {
    pub fn read() -> Option<super::Focus> {
        None
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn hyprland_gives_the_class_and_the_title_and_nothing_else() {
        let json = br#"{"address":"0x1","class":"foot","initialClass":"foot","title":"~/src","pid":42,"workspace":{"id":1}}"#;
        assert_eq!(
            platform::parse(json),
            Some(Focus {
                app: "foot".into(),
                title: "~/src".into()
            })
        );
        let json = br#"{"class":"","initialClass":"Slack","title":"general"}"#;
        assert_eq!(platform::parse(json).unwrap().app, "Slack");
        // No window focused: hyprctl prints `{}`.
        assert_eq!(platform::parse(b"{}"), Some(Focus::default()));
        assert_eq!(platform::parse(b"Invalid"), None);
    }
}
