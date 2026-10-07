//! The mouse's back and forward buttons, on Linux.
//!
//! WebKitGTK gives the page no way to tell them apart: both arrive as
//! button 0, and the web view doesn't go back by itself either. So the app
//! catches buttons 8 and 9 on the web view before WebKit does and tells the
//! page with a `sol:navigate` event (`detail` is `back` or `forward`), which
//! sol-design's Shell answers like Alt+← and Alt+→. Elsewhere (WebView2)
//! the page sees the buttons itself.

use std::ffi::{c_int, c_void};

use crate::frames::symbol;

type Connect = unsafe extern "C" fn(
    *mut c_void,
    *const std::ffi::c_char,
    *mut c_void,
    *mut c_void,
    *mut c_void,
    c_int,
) -> std::ffi::c_ulong;
type EventButton = unsafe extern "C" fn(*const c_void, *mut u32) -> c_int;
#[allow(clippy::type_complexity)]
type Evaluate = unsafe extern "C" fn(
    *mut c_void,
    *const std::ffi::c_char,
    isize,
    *const std::ffi::c_char,
    *const std::ffi::c_char,
    *mut c_void,
    *mut c_void,
    *mut c_void,
);

/// The event the page hears, by button.
const BACK: &std::ffi::CStr =
    c"window.dispatchEvent(new CustomEvent('sol:navigate', { detail: 'back' }))";
const FORWARD: &std::ffi::CStr =
    c"window.dispatchEvent(new CustomEvent('sol:navigate', { detail: 'forward' }))";

/// GTK's `button-press-event` on the web view: 8 is back, 9 forward.
unsafe extern "C" fn pressed(web_view: *mut c_void, event: *const c_void, _: *mut c_void) -> c_int {
    // SAFETY: GTK calls this on the main thread with the live widget and
    // its event; the functions are WebKitGTK's and GDK's own.
    unsafe {
        let (Some(button_of), Some(evaluate)) = (
            symbol::<EventButton>(libc::RTLD_DEFAULT, c"gdk_event_get_button"),
            symbol::<Evaluate>(libc::RTLD_DEFAULT, c"webkit_web_view_evaluate_javascript"),
        ) else {
            return 0;
        };
        let mut button = 0;
        if button_of(event, &mut button) == 0 {
            return 0;
        }
        let script = match button {
            8 => BACK,
            9 => FORWARD,
            _ => return 0,
        };
        let (none, null) = (std::ptr::null(), std::ptr::null_mut());
        evaluate(web_view, script.as_ptr(), -1, none, none, null, null, null);
        1
    }
}

/// GTK's `button-release-event`: the second half of 8 and 9 stays here too,
/// or the page sees a stray release of its main button.
unsafe extern "C" fn released(_: *mut c_void, event: *const c_void, _: *mut c_void) -> c_int {
    // SAFETY: GTK calls this on the main thread with a live event.
    unsafe {
        let Some(button_of) = symbol::<EventButton>(libc::RTLD_DEFAULT, c"gdk_event_get_button")
        else {
            return 0;
        };
        let mut button = 0;
        c_int::from(button_of(event, &mut button) != 0 && matches!(button, 8 | 9))
    }
}

/// Passes the mouse's back and forward buttons on to the page, as
/// `sol:navigate` events. `web_view` is a `WebKitWebView*` (a Tauri
/// window's `with_webview` hands one out).
///
/// # Safety
///
/// `web_view` must be a live `WebKitWebView`, used on the main thread.
pub unsafe fn back_and_forward(web_view: *mut c_void) {
    // SAFETY: GLib's own function, by name, with the caller's live widget.
    unsafe {
        let Some(connect) = symbol::<Connect>(libc::RTLD_DEFAULT, c"g_signal_connect_data") else {
            return;
        };
        type Handler = unsafe extern "C" fn(*mut c_void, *const c_void, *mut c_void) -> c_int;
        for (signal, handler) in [
            (c"button-press-event", pressed as Handler),
            (c"button-release-event", released as Handler),
        ] {
            connect(
                web_view,
                signal.as_ptr(),
                handler as *mut c_void,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            );
        }
    }
}
