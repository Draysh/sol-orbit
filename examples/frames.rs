//! `orbit::frames` on its own, as a shared library to preload into a
//! WebKitGTK window that isn't a world's app, e.g. sol-design's
//! `scripts/scrollbench.py`, so it measures what the apps really do:
//!
//! ```text
//! cargo build --release --features app --example frames
//! LD_PRELOAD=target/release/examples/libframes.so scripts/scrollbench.py …
//! ```

/// Referenced here so the linker keeps the shim's symbol in this library.
#[used]
static DRM_WAIT_VBLANK: unsafe extern "C" fn(
    std::ffi::c_int,
    *mut orbit::frames::DrmVBlank,
) -> std::ffi::c_int = orbit::frames::drmWaitVBlank;

/// What an app does once its web view exists ([`orbit::frames::full_rate`]),
/// for the bench to call on its own (`ctypes`).
///
/// # Safety
/// `web_view` is a live `WebKitWebView`, on the main thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn orbit_frames_full_rate(web_view: *mut std::ffi::c_void) -> bool {
    // SAFETY: as the caller promises.
    unsafe { orbit::frames::full_rate(web_view) }
}

/// The mouse's back and forward buttons ([`orbit::mouse::back_and_forward`]),
/// for a test window to call on its own (`ctypes`).
///
/// # Safety
/// `web_view` is a live `WebKitWebView`, on the main thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn orbit_mouse_back_and_forward(web_view: *mut std::ffi::c_void) {
    // SAFETY: as the caller promises.
    unsafe { orbit::mouse::back_and_forward(web_view) }
}
