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
