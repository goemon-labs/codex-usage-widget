use std::{cell::Cell, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::HWND,
    UI::{
        Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent},
        WindowsAndMessaging::{
            EVENT_SYSTEM_FOREGROUND, GWL_EXSTYLE, GetWindowLongW, HWND_NOTOPMOST, HWND_TOPMOST,
            IsIconic, IsWindowVisible, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
            WINEVENT_OUTOFCONTEXT, WS_EX_TOPMOST,
        },
    },
};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

// OUTOFCONTEXT callbacks run on the registering UI thread. There is one widget window.
thread_local! {
    static PINNED_WINDOW: Cell<HWND> = const { Cell::new(null_mut()) };
}

pub struct Topmost {
    window: HWND,
    hook: HWINEVENTHOOK,
}

impl Topmost {
    pub fn new(window: &winit::window::Window, enabled: bool) -> Option<Self> {
        let RawWindowHandle::Win32(handle) = window.window_handle().ok()?.as_raw() else {
            return None;
        };
        // The callback and hook live on the same thread as the native window/message loop.
        let hook = unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                null_mut(),
                Some(foreground_changed),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            )
        };
        let guard = Self {
            window: handle.hwnd.get() as HWND,
            hook,
        };
        guard.set_enabled(enabled);
        Some(guard)
    }

    pub fn set_enabled(&self, enabled: bool) {
        PINNED_WINDOW.set(if enabled { self.window } else { null_mut() });
        apply(self.window, enabled);
    }
}

fn apply(window: HWND, enabled: bool) {
    // Apply directly: winit skips repeated requests based on its cached window flags,
    // even when Windows' actual stacking order no longer matches WS_EX_TOPMOST.
    unsafe {
        SetWindowPos(
            window,
            if enabled {
                HWND_TOPMOST
            } else {
                HWND_NOTOPMOST
            },
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

unsafe extern "system" fn foreground_changed(
    _hook: HWINEVENTHOOK,
    _event: u32,
    foreground: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    let window = PINNED_WINDOW.get();
    if window.is_null() || foreground.is_null() {
        return;
    }
    // Respect hidden/minimized state and other apps explicitly using topmost.
    // NOACTIVATE preserves the app the user is currently typing in.
    unsafe {
        if IsWindowVisible(window) == 0 || IsIconic(window) != 0 {
            return;
        }
        if foreground != window
            && GetWindowLongW(foreground, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST != 0
        {
            return;
        }
    }
    apply(window, true);
}

impl Drop for Topmost {
    fn drop(&mut self) {
        PINNED_WINDOW.set(null_mut());
        if !self.hook.is_null() {
            // Unregister on the same UI thread before releasing the guard.
            unsafe { UnhookWinEvent(self.hook) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, GW_HWNDPREV, GetForegroundWindow, GetWindow, SW_HIDE,
        SW_SHOWNOACTIVATE, ShowWindow, WS_POPUP,
    };

    struct TestWindow(HWND);

    impl TestWindow {
        fn new() -> Self {
            // A tiny off-screen native window exercises Windows without interrupting input.
            let window = unsafe {
                CreateWindowExW(
                    0,
                    windows_sys::core::w!("STATIC"),
                    windows_sys::core::w!(""),
                    WS_POPUP,
                    -32000,
                    -32000,
                    1,
                    1,
                    null_mut(),
                    null_mut(),
                    null_mut(),
                    std::ptr::null(),
                )
            };
            assert!(!window.is_null());
            unsafe { ShowWindow(window, SW_SHOWNOACTIVATE) };
            Self(window)
        }
    }

    impl Drop for TestWindow {
        fn drop(&mut self) {
            unsafe { DestroyWindow(self.0) };
        }
    }

    #[test]
    fn foreground_repair_preserves_focus_and_respects_disabled_hidden_and_topmost_apps() {
        let widget = TestWindow::new();
        let other = TestWindow::new();
        let guard = Topmost {
            window: widget.0,
            hook: null_mut(),
        };
        let notify = || unsafe {
            foreground_changed(null_mut(), EVENT_SYSTEM_FOREGROUND, other.0, 0, 0, 0, 0);
        };
        let is_topmost =
            || unsafe { GetWindowLongW(widget.0, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST != 0 };
        guard.set_enabled(true);
        apply(widget.0, false);
        let foreground = unsafe { GetForegroundWindow() };
        notify();
        assert!(is_topmost());
        assert_eq!(unsafe { GetForegroundWindow() }, foreground);
        // Check actual ordering as well as the style bit.
        let mut previous = unsafe { GetWindow(widget.0, GW_HWNDPREV) };
        while !previous.is_null() {
            assert_ne!(previous, other.0);
            previous = unsafe { GetWindow(previous, GW_HWNDPREV) };
        }

        apply(widget.0, false);
        apply(other.0, true);
        notify();
        assert!(!is_topmost());
        apply(other.0, false);

        unsafe { ShowWindow(widget.0, SW_HIDE) };
        notify();
        assert!(!is_topmost());
        unsafe { ShowWindow(widget.0, SW_SHOWNOACTIVATE) };

        guard.set_enabled(false);
        notify();
        assert!(!is_topmost());
    }
}
