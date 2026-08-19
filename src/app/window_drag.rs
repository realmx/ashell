//! Window dragging helpers for the integrated title bar.
//!
//! GPUI implements `Window::start_window_move` on macOS and Linux, but the
//! Windows backend leaves it as a no-op. On Windows we drive the native move
//! loop ourselves with `WM_SYSCOMMAND` / `SC_MOVE`, which is the same mechanism
//! `DefWindowProc` uses for `HTCAPTION` drags.

/// Begin a native window drag from a mouse-down on the integrated title bar.
pub(crate) fn start_window_drag(window: &gpui::Window) {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
        use windows::Win32::UI::WindowsAndMessaging::{
            SC_MOVE, SendMessageW, WM_SYSCOMMAND, WPARAM,
        };

        let Some(handle) = crate::desktop_notification::native_window_handle(window) else {
            tracing::warn!("failed to start window drag: missing native window handle");
            return;
        };

        unsafe {
            // The button-down capture must be released before the system move
            // loop can take over the pointer.
            let _ = ReleaseCapture();
            SendMessageW(
                HWND(handle as _),
                WM_SYSCOMMAND,
                WPARAM(SC_MOVE as usize),
                None,
            );
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        window.start_window_move();
    }
}
