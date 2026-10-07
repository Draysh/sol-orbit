//! Frames at the screen's rate on Linux.
//!
//! Every world's window is WebKitGTK, and WebKit paces its compositor on the
//! display's vertical blank, which it waits for with libdrm's `drmWaitVBlank`.
//! Where that call fails (NVIDIA's proprietary driver doesn't implement it),
//! WebKit falls back to a timer fixed at 60 frames a second whatever the
//! monitor does, and no setting raises it: a 170 Hz screen shows every
//! frame for two or three refreshes and scrolling judders.
//!
//! So an app supplies its own [`drmWaitVBlank`]. It calls libdrm's first,
//! and drivers that answer are left alone. When the call fails, it reads the
//! CRTC's mode and sleeps to the next frame boundary at that refresh rate.
//! WebKit's UI process is the app itself, so once the binary exports the
//! symbol, WebKit binds to ours instead of libdrm's. That takes two lines in
//! the app's `build.rs`, which keep the symbol and export it:
//!
//! ```text
//! println!("cargo:rustc-link-arg-bins=-Wl,--undefined=drmWaitVBlank");
//! println!("cargo:rustc-link-arg-bins=-Wl,--export-dynamic-symbol=drmWaitVBlank");
//! ```
//!
//! WebKit's second cap is its own preference for page rendering updates near
//! 60 a second (an even fraction of the screen's rate: 85 on 170 Hz), which
//! [`full_rate`] turns off for a web view once it exists.
//!
//! The third cap, and on a driver with vertical blanks the one that matters,
//! is GTK 3 itself. The window draws WebKit's frames with GL, and on Wayland
//! GTK 3 then never asks the compositor for frame callbacks (it does only
//! for the windows it commits itself), so its frame timings never learn the
//! screen's refresh interval and its frame clock spaces paints by its
//! built-in `FRAME_INTERVAL`, 16 667 µs: 60 a second, whatever WebKit makes.
//! [`full_rate`] also tells the window's frame clock the monitor's real
//! interval after every paint, so GTK paints at the screen's rate; the GL
//! swap still waits for the compositor, which keeps it on the vblank.
//!
//! `WEBKIT_FORCE_VBLANK_TIMER=1` brings WebKit's 60 Hz timer back, to compare.
//! The `frames` example builds this module alone as a shared library, to
//! preload into a WebKitGTK window that isn't a world's app (sol-design's
//! `scripts/scrollbench.py`): `LD_PRELOAD=target/release/examples/libframes.so`.

use std::{
    collections::HashMap,
    ffi::{CStr, c_char, c_int, c_long, c_ulong, c_void},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

/// `drmVBlankSeqType`: wait until the count reaches the sequence …
const ABSOLUTE: u32 = 0x0;
/// … or until it has gone up by the sequence.
const RELATIVE: u32 = 0x1;
/// The CRTC's index, for the third one up, sits in these bits.
const HIGH_CRTC_MASK: u32 = 0x3e;
const HIGH_CRTC_SHIFT: u32 = 1;
/// Deliver a DRM event instead of blocking.
const EVENT: u32 = 0x0400_0000;
/// If the sequence has passed, wait for the next vblank instead.
const NEXT_ON_MISS: u32 = 0x1000_0000;
/// The second CRTC, the old way.
const SECONDARY: u32 = 0x2000_0000;
/// Send a signal when done (long gone from the kernel).
const SIGNAL: u32 = 0x4000_0000;

/// `drm_mode.h`: the mode's flags that change its line count.
const MODE_INTERLACE: u32 = 1 << 4;
const MODE_DBLSCAN: u32 = 1 << 5;

/// What a driver without vertical blanks is taken to run at.
const DEFAULT_HZ: f64 = 60.0;

/// `struct drm_wait_vblank_request`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Request {
    kind: u32,
    sequence: u32,
    signal: c_ulong,
}

/// `struct drm_wait_vblank_reply`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Reply {
    kind: u32,
    sequence: u32,
    tval_sec: c_long,
    tval_usec: c_long,
}

/// libdrm's `drmVBlank`: the request going in, the reply coming out.
#[repr(C)]
pub union DrmVBlank {
    request: Request,
    reply: Reply,
}

/// libdrm's `drmModeModeInfo`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct ModeInfo {
    clock: u32,
    hdisplay: u16,
    hsync_start: u16,
    hsync_end: u16,
    htotal: u16,
    hskew: u16,
    vdisplay: u16,
    vsync_start: u16,
    vsync_end: u16,
    vtotal: u16,
    vscan: u16,
    vrefresh: u32,
    flags: u32,
    kind: u32,
    name: [c_char; 32],
}

/// libdrm's `drmModeCrtc`.
#[repr(C)]
struct Crtc {
    crtc_id: u32,
    buffer_id: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    mode_valid: c_int,
    mode: ModeInfo,
    gamma_size: c_int,
}

/// libdrm's `drmModeRes`.
#[repr(C)]
struct Resources {
    count_fbs: c_int,
    fbs: *mut u32,
    count_crtcs: c_int,
    crtcs: *mut u32,
    count_connectors: c_int,
    connectors: *mut u32,
    count_encoders: c_int,
    encoders: *mut u32,
    min_width: u32,
    max_width: u32,
    min_height: u32,
    max_height: u32,
}

type WaitVBlank = unsafe extern "C" fn(c_int, *mut DrmVBlank) -> c_int;
type GetResources = unsafe extern "C" fn(c_int) -> *mut Resources;
type FreeResources = unsafe extern "C" fn(*mut Resources);
type GetCrtc = unsafe extern "C" fn(c_int, u32) -> *mut Crtc;
type FreeCrtc = unsafe extern "C" fn(*mut Crtc);

/// libdrm's own functions, looked up once. They are all in the process
/// already (WebKit links libdrm), so there is nothing to load.
struct Libdrm {
    wait: Option<WaitVBlank>,
    get_resources: Option<GetResources>,
    free_resources: Option<FreeResources>,
    get_crtc: Option<GetCrtc>,
    free_crtc: Option<FreeCrtc>,
}

pub(crate) unsafe fn symbol<T: Copy>(handle: *mut c_void, name: &CStr) -> Option<T> {
    // SAFETY: dlsym takes a C string and returns a pointer or null; the
    // caller names the function's real type.
    let ptr = unsafe { libc::dlsym(handle, name.as_ptr()) };
    (!ptr.is_null()).then(|| unsafe { std::mem::transmute_copy::<*mut c_void, T>(&ptr) })
}

fn libdrm() -> &'static Libdrm {
    static LIBDRM: OnceLock<Libdrm> = OnceLock::new();
    LIBDRM.get_or_init(|| unsafe {
        Libdrm {
            // The next definition after this one: libdrm's.
            wait: symbol(libc::RTLD_NEXT, c"drmWaitVBlank"),
            get_resources: symbol(libc::RTLD_DEFAULT, c"drmModeGetResources"),
            free_resources: symbol(libc::RTLD_DEFAULT, c"drmModeFreeResources"),
            get_crtc: symbol(libc::RTLD_DEFAULT, c"drmModeGetCrtc"),
            free_crtc: symbol(libc::RTLD_DEFAULT, c"drmModeFreeCrtc"),
        }
    })
}

/// The CRTC a request is about: its index among the device's.
fn crtc_index(kind: u32) -> u32 {
    if kind & SECONDARY != 0 {
        1
    } else {
        (kind & HIGH_CRTC_MASK) >> HIGH_CRTC_SHIFT
    }
}

/// A mode's refresh rate in Hz, the way the kernel works it out
/// (`drm_mode_vrefresh`): pixel clock over the lines and columns scanned.
fn mode_rate(mode: &ModeInfo) -> Option<f64> {
    if mode.htotal == 0 || mode.vtotal == 0 {
        return (mode.vrefresh > 0).then_some(f64::from(mode.vrefresh));
    }
    let mut num = f64::from(mode.clock) * 1000.0;
    let mut den = f64::from(mode.htotal) * f64::from(mode.vtotal);
    if mode.flags & MODE_INTERLACE != 0 {
        num *= 2.0;
    }
    if mode.flags & MODE_DBLSCAN != 0 {
        den *= 2.0;
    }
    if mode.vscan > 1 {
        den *= f64::from(mode.vscan);
    }
    let rate = num / den;
    if rate.is_finite() && rate >= 1.0 {
        Some(rate)
    } else {
        (mode.vrefresh > 0).then_some(f64::from(mode.vrefresh))
    }
}

/// The refresh rate of the CRTC at `index` on the device behind `fd`, from
/// the mode it is showing.
fn crtc_rate(fd: c_int, index: u32) -> Option<f64> {
    let lib = libdrm();
    let (get_resources, free_resources, get_crtc, free_crtc) = (
        lib.get_resources?,
        lib.free_resources?,
        lib.get_crtc?,
        lib.free_crtc?,
    );
    // SAFETY: libdrm's own functions with the pointers they hand out, each
    // freed by its pair.
    unsafe {
        let resources = get_resources(fd);
        if resources.is_null() {
            return None;
        }
        let count = usize::try_from((*resources).count_crtcs).unwrap_or(0);
        let id = (index as usize) < count && !(*resources).crtcs.is_null();
        let id = id.then(|| *(*resources).crtcs.add(index as usize));
        free_resources(resources);
        let crtc = get_crtc(fd, id?);
        if crtc.is_null() {
            return None;
        }
        let rate = ((*crtc).mode_valid != 0)
            .then(|| mode_rate(&(*crtc).mode))
            .flatten();
        free_crtc(crtc);
        rate
    }
}

/// Vertical blanks made up for one CRTC: a steady beat since the first
/// request, so waits don't drift with how late each sleep wakes.
struct Beat {
    origin: Instant,
    period: Duration,
}

impl Beat {
    fn new(hz: f64) -> Self {
        Beat {
            origin: Instant::now(),
            period: Duration::from_secs_f64(1.0 / hz),
        }
    }

    /// How many blanks have passed by `at`.
    fn count(&self, at: Instant) -> u64 {
        let elapsed = at.saturating_duration_since(self.origin);
        (elapsed.as_secs_f64() / self.period.as_secs_f64()) as u64
    }

    fn at(&self, sequence: u64) -> Instant {
        self.origin + self.period.mul_f64(sequence as f64)
    }
}

/// The blank a request waits for, given the count now: `None` is "right
/// away, with the count as it is". Relative requests wait for the count to
/// go up by the sequence, absolute ones for it to reach the sequence; one
/// already reached returns at once unless the request asks for the next.
fn target(kind: u32, sequence: u32, now: u64) -> Option<u64> {
    let wanted = if kind & RELATIVE != 0 {
        now + u64::from(sequence)
    } else {
        u64::from(sequence)
    };
    if wanted > now {
        Some(wanted)
    } else if kind & NEXT_ON_MISS != 0 {
        Some(now + 1)
    } else {
        None
    }
}

fn beats() -> &'static Mutex<HashMap<(c_int, u32), Beat>> {
    static BEATS: OnceLock<Mutex<HashMap<(c_int, u32), Beat>>> = OnceLock::new();
    BEATS.get_or_init(Mutex::default)
}

/// Says what the shim is doing: through tracing where the app set it up,
/// to stderr otherwise, so a terminal shows it either way.
fn say(line: String) {
    if tracing::dispatcher::has_been_set() {
        tracing::info!("{line}");
    } else {
        eprintln!("orbit::frames: {line}");
    }
}

fn errno() -> c_int {
    // SAFETY: errno's address is always valid to read.
    unsafe { *libc::__errno_location() }
}

fn set_errno(err: c_int) {
    // SAFETY: errno's address is always valid to write.
    unsafe { *libc::__errno_location() = err };
}

/// Fills in the reply the way the kernel would: the count reached, and when.
fn reply(vbl: *mut DrmVBlank, kind: u32, sequence: u64) {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a valid timespec to fill in.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    // libdrm clears the relative bit before returning; so do we.
    let reply = Reply {
        kind: kind & !RELATIVE,
        sequence: sequence as u32,
        tval_sec: now.tv_sec,
        tval_usec: now.tv_nsec / 1000,
    };
    // SAFETY: the caller checked the pointer.
    unsafe { (*vbl).reply = reply };
}

/// Waits for a vertical blank the way the driver would have, when the
/// driver won't: the next one at the CRTC's refresh rate.
fn pretend(fd: c_int, request: Request, vbl: *mut DrmVBlank, why: c_int) -> c_int {
    let crtc = crtc_index(request.kind);
    let key = (fd, crtc);
    let (deadline, sequence) = {
        let mut beats = beats().lock().unwrap_or_else(|e| e.into_inner());
        let beat = beats.entry(key).or_insert_with(|| {
            let (hz, from) = match crtc_rate(fd, crtc) {
                Some(hz) => (hz, "the display's mode"),
                None => (DEFAULT_HZ, "nothing better"),
            };
            let error = std::io::Error::from_raw_os_error(why);
            say(format!(
                "drmWaitVBlank failed on fd {fd}, crtc {crtc} ({error}); pacing frames at {hz:.2} Hz from {from}"
            ));
            Beat::new(hz)
        });
        let now = beat.count(Instant::now());
        match target(request.kind, request.sequence, now) {
            Some(sequence) => (Some(beat.at(sequence)), sequence),
            None => (None, now),
        }
    };
    if let Some(deadline) = deadline {
        let now = Instant::now();
        if deadline > now {
            std::thread::sleep(deadline - now);
        }
    }
    reply(vbl, request.kind, sequence);
    0
}

/// libdrm's `drmWaitVBlank`, with a fallback: WebKit calls this one once the
/// app's binary exports it (see the module's notes). Drivers that answer
/// are left alone; where the call fails, the wait is made up from the
/// CRTC's refresh rate instead of leaving WebKit to its 60 Hz timer.
///
/// # Safety
///
/// `vbl` must point to a `drmVBlank`, or be null.
#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drmWaitVBlank(fd: c_int, vbl: *mut DrmVBlank) -> c_int {
    if vbl.is_null() {
        set_errno(libc::EINVAL);
        return -1;
    }
    // SAFETY: not null, and the caller's drmVBlank.
    let request = unsafe { (*vbl).request };
    let lib = libdrm();
    let err = match lib.wait {
        Some(real) => {
            // SAFETY: libdrm's own function with the caller's arguments.
            let ret = unsafe { real(fd, vbl) };
            if ret == 0 {
                return 0;
            }
            let err = errno();
            // The kernel says when it has gone away or been interrupted;
            // only a driver that can't do it at all is replaced.
            if matches!(err, libc::EINTR | libc::EBUSY) {
                return ret;
            }
            err
        }
        None => libc::ENOSYS,
    };
    // An event or a signal would have to come from the kernel, and so would
    // anything else this doesn't know; nothing to make up.
    const KNOWN: u32 = ABSOLUTE | RELATIVE | HIGH_CRTC_MASK | NEXT_ON_MISS | SECONDARY;
    if request.kind & (EVENT | SIGNAL) != 0 || request.kind & !KNOWN != 0 {
        set_errno(err);
        return -1;
    }
    pretend(fd, request, vbl, err)
}

type ViewSettings = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type AllFeatures = unsafe extern "C" fn() -> *mut c_void;
type ListLength = unsafe extern "C" fn(*mut c_void) -> usize;
type ListGet = unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void;
type ListUnref = unsafe extern "C" fn(*mut c_void);
type FeatureIdentifier = unsafe extern "C" fn(*mut c_void) -> *const c_char;
type SetFeature = unsafe extern "C" fn(*mut c_void, *mut c_void, c_int);

/// WebKit's own cap: page rendering updates kept near 60 a second.
const NEAR_60: &CStr = c"PreferPageRenderingUpdatesNear60FPS";

/// WebKit features [`full_rate`] turns off: the cap near 60 a second, and
/// WebKitGTK 2.54's compositing of only the damaged region, which leaves
/// the window's buffers holding different pictures of parts of the page
/// that aren't changing: hovering a cover made it flash. Compositing the
/// whole frame costs nothing measurable (157 against 159 frames a second
/// scrolling on a 170 Hz screen).
const OFF: [&CStr; 3] = [
    NEAR_60,
    c"UseDamagingInformationForCompositing",
    c"PropagateDamagingInformation",
];

// GTK 3, for its frame clock.
type GetFrameClock = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type GetWindow = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type CurrentTimings = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type WindowDisplay = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type MonitorAt = unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void;
type RefreshRate = unsafe extern "C" fn(*mut c_void) -> c_int;
type MajorVersion = unsafe extern "C" fn() -> u32;
type Connect = unsafe extern "C" fn(
    *mut c_void,
    *const c_char,
    *mut c_void,
    *mut c_void,
    *mut c_void,
    c_int,
) -> std::ffi::c_ulong;
type GetData = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
type SetData = unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void);

/// Where `refresh_interval` sits in GTK 3's private `GdkFrameTimings`: a
/// `guint` (padded to 8) and six `gint64`s before it, unchanged since 3.8.
const REFRESH_INTERVAL_AT: usize = 56;
const PACED: &CStr = c"orbit-frames-paced";

/// GTK 3's functions this needs, looked up once.
struct Gtk3 {
    frame_clock: GetFrameClock,
    window: GetWindow,
    timings: CurrentTimings,
    display: WindowDisplay,
    monitor_at: MonitorAt,
    refresh_rate: RefreshRate,
    connect: Connect,
    get_data: GetData,
    set_data: SetData,
}

fn gtk3() -> Option<&'static Gtk3> {
    static GTK: OnceLock<Option<Gtk3>> = OnceLock::new();
    GTK.get_or_init(|| {
        // SAFETY: GTK's and GLib's own functions, by name, in a process that
        // has them loaded (it runs a WebKitGTK view).
        unsafe {
            let major = symbol::<MajorVersion>(libc::RTLD_DEFAULT, c"gtk_get_major_version")?;
            if major() != 3 {
                return None;
            }
            Some(Gtk3 {
                frame_clock: symbol(libc::RTLD_DEFAULT, c"gtk_widget_get_frame_clock")?,
                window: symbol(libc::RTLD_DEFAULT, c"gtk_widget_get_window")?,
                timings: symbol(libc::RTLD_DEFAULT, c"gdk_frame_clock_get_current_timings")?,
                display: symbol(libc::RTLD_DEFAULT, c"gdk_window_get_display")?,
                monitor_at: symbol(libc::RTLD_DEFAULT, c"gdk_display_get_monitor_at_window")?,
                refresh_rate: symbol(libc::RTLD_DEFAULT, c"gdk_monitor_get_refresh_rate")?,
                connect: symbol(libc::RTLD_DEFAULT, c"g_signal_connect_data")?,
                get_data: symbol(libc::RTLD_DEFAULT, c"g_object_get_data")?,
                set_data: symbol(libc::RTLD_DEFAULT, c"g_object_set_data")?,
            })
        }
    })
    .as_ref()
}

/// The monitor's refresh interval in microseconds, for the widget's window.
///
/// # Safety
/// `widget` is a live, realized `GtkWidget`.
unsafe fn refresh_interval(gtk: &Gtk3, widget: *mut c_void) -> Option<i64> {
    // SAFETY: as the caller promises; every pointer comes from GTK.
    unsafe {
        let window = (gtk.window)(widget);
        if window.is_null() {
            return None;
        }
        let monitor = (gtk.monitor_at)((gtk.display)(window), window);
        if monitor.is_null() {
            return None;
        }
        let mhz = (gtk.refresh_rate)(monitor);
        (mhz > 0).then(|| 1_000_000_000 / i64::from(mhz))
    }
}

/// After each paint: the frame's timings learn the screen's interval, so
/// the next frame may come that soon.
unsafe extern "C" fn after_paint(clock: *mut c_void, widget: *mut c_void) {
    let Some(gtk) = gtk3() else { return };
    // SAFETY: GTK calls this on the main thread with its live clock, and
    // `widget` (the web view) outlives its window's clock.
    unsafe {
        let timings = (gtk.timings)(clock);
        if timings.is_null() {
            return;
        }
        if let Some(interval) = refresh_interval(gtk, widget) {
            *timings.cast::<u8>().add(REFRESH_INTERVAL_AT).cast::<i64>() = interval;
        }
    }
}

/// Once the web view has a frame clock, paces it (once per clock).
unsafe extern "C" fn pace(widget: *mut c_void) {
    let Some(gtk) = gtk3() else { return };
    // SAFETY: a live widget on the main thread (a signal handler of its own,
    // or the caller of `full_rate`).
    unsafe {
        let clock = (gtk.frame_clock)(widget);
        if clock.is_null() || !(gtk.get_data)(clock, PACED.as_ptr()).is_null() {
            return;
        }
        // Any non-null pointer marks it.
        (gtk.set_data)(clock, PACED.as_ptr(), PACED.as_ptr().cast_mut().cast());
        let handler: unsafe extern "C" fn(*mut c_void, *mut c_void) = after_paint;
        (gtk.connect)(
            clock,
            c"after-paint".as_ptr(),
            handler as *mut c_void,
            widget,
            std::ptr::null_mut(),
            0,
        );
    }
    say("GTK paints at the screen's rate".into());
}

/// Paces the window's frame clock at the screen's rate, now if the web view
/// is on screen, or else when it gets there.
///
/// # Safety
/// `web_view` is a live `GtkWidget`, on the main thread.
unsafe fn pace_gtk(web_view: *mut c_void) {
    let Some(gtk) = gtk3() else { return };
    // SAFETY: as the caller promises.
    unsafe {
        pace(web_view);
        let handler: unsafe extern "C" fn(*mut c_void) = pace;
        (gtk.connect)(
            web_view,
            c"map".as_ptr(),
            handler as *mut c_void,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        );
    }
}

/// Lets a web view update its page at the screen's rate instead of a
/// fraction near 60 a second, WebKit's default, and its window paint at
/// that rate too (GTK 3's frame clock, see the module's notes); and turns
/// off WebKitGTK 2.54's damage-only compositing, which makes still parts of
/// the page flash ([`OFF`]). `web_view`
/// is a `WebKitWebView*` (a Tauri window's `with_webview` hands one out).
/// Returns whether WebKit's preference was found; WebKitGTK before 2.42 has
/// no way to set it.
///
/// # Safety
///
/// `web_view` must be a live `WebKitWebView`, used on the main thread.
pub unsafe fn full_rate(web_view: *mut c_void) -> bool {
    // SAFETY: WebKitGTK's own functions, looked up by name in the process
    // that already runs the web view, with the pointers they hand out.
    unsafe {
        pace_gtk(web_view);
        let Some(settings_of) =
            symbol::<ViewSettings>(libc::RTLD_DEFAULT, c"webkit_web_view_get_settings")
        else {
            return false;
        };
        let (Some(all), Some(length), Some(get), Some(unref), Some(identifier), Some(set)) = (
            symbol::<AllFeatures>(libc::RTLD_DEFAULT, c"webkit_settings_get_all_features"),
            symbol::<ListLength>(libc::RTLD_DEFAULT, c"webkit_feature_list_get_length"),
            symbol::<ListGet>(libc::RTLD_DEFAULT, c"webkit_feature_list_get"),
            symbol::<ListUnref>(libc::RTLD_DEFAULT, c"webkit_feature_list_unref"),
            symbol::<FeatureIdentifier>(libc::RTLD_DEFAULT, c"webkit_feature_get_identifier"),
            symbol::<SetFeature>(libc::RTLD_DEFAULT, c"webkit_settings_set_feature_enabled"),
        ) else {
            say("this WebKitGTK can't be asked for page updates at the screen's rate".into());
            return false;
        };
        let settings = settings_of(web_view);
        if settings.is_null() {
            return false;
        }
        let features = all();
        if features.is_null() {
            return false;
        }
        let mut found = false;
        for i in 0..length(features) {
            let feature = get(features, i);
            if feature.is_null() {
                continue;
            }
            let id = identifier(feature);
            if id.is_null() {
                continue;
            }
            let id = CStr::from_ptr(id);
            if OFF.contains(&id) {
                set(settings, feature, 0);
                found |= id == NEAR_60;
            }
        }
        unref(features);
        if found {
            say("page updates at the screen's rate".into());
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(clock: u32, htotal: u16, vtotal: u16, flags: u32) -> ModeInfo {
        ModeInfo {
            clock,
            htotal,
            vtotal,
            flags,
            ..ModeInfo::default()
        }
    }

    #[test]
    fn rates_come_from_the_mode_timings() {
        // 1080p60 as CTA-861 lists it, and 1080i60 (two fields a frame).
        assert_eq!(
            mode_rate(&mode(148_500, 2200, 1125, 0)).map(f64::round),
            Some(60.0)
        );
        assert_eq!(
            mode_rate(&mode(74_250, 2200, 1125, MODE_INTERLACE)).map(f64::round),
            Some(60.0)
        );
        // A 2560×1440 at 170 Hz, roughly: the rate is not rounded to whole Hz.
        let rate = mode_rate(&mode(730_000, 2720, 1577, 0)).unwrap();
        assert!((rate - 170.19).abs() < 0.01, "{rate}");
        // Doublescan halves it; a mode with no timings falls back to its own figure.
        assert_eq!(
            mode_rate(&mode(148_500, 2200, 1125, MODE_DBLSCAN)).map(f64::round),
            Some(30.0)
        );
        let bare = ModeInfo {
            vrefresh: 144,
            ..ModeInfo::default()
        };
        assert_eq!(mode_rate(&bare), Some(144.0));
        assert_eq!(mode_rate(&ModeInfo::default()), None);
    }

    #[test]
    fn requests_name_their_crtc() {
        assert_eq!(crtc_index(RELATIVE), 0);
        assert_eq!(crtc_index(RELATIVE | SECONDARY), 1);
        assert_eq!(crtc_index(RELATIVE | (3 << HIGH_CRTC_SHIFT)), 3);
    }

    #[test]
    fn targets_follow_the_kernel() {
        assert_eq!(target(RELATIVE, 1, 10), Some(11));
        assert_eq!(target(RELATIVE, 0, 10), None);
        assert_eq!(target(ABSOLUTE, 12, 10), Some(12));
        assert_eq!(target(ABSOLUTE, 9, 10), None);
        assert_eq!(target(ABSOLUTE | NEXT_ON_MISS, 9, 10), Some(11));
    }

    #[test]
    fn a_beat_keeps_time() {
        let beat = Beat::new(100.0);
        assert_eq!(beat.count(beat.origin), 0);
        assert_eq!(beat.count(beat.origin + Duration::from_millis(25)), 2);
        assert_eq!(beat.at(4), beat.origin + Duration::from_millis(40));
    }

    /// With no driver at all (this test binary links no libdrm), waiting
    /// still works: a beat at 60 Hz, counted up one request at a time.
    #[test]
    fn waits_without_a_driver() {
        let mut vbl = DrmVBlank {
            request: Request {
                kind: RELATIVE | (7 << HIGH_CRTC_SHIFT),
                sequence: 1,
                signal: 0,
            },
        };
        let start = Instant::now();
        for expected in 1..=6u32 {
            unsafe {
                vbl.request = Request {
                    kind: RELATIVE | (7 << HIGH_CRTC_SHIFT),
                    sequence: 1,
                    signal: 0,
                };
                assert_eq!(drmWaitVBlank(-1, &mut vbl), 0);
                assert_eq!(vbl.reply.sequence, expected);
                assert_eq!(vbl.reply.kind & RELATIVE, 0);
            }
        }
        let took = start.elapsed();
        // Six blanks at 60 Hz: 100 ms, give or take the scheduler.
        assert!(took >= Duration::from_millis(95), "{took:?}");
        assert!(took < Duration::from_millis(250), "{took:?}");

        // A request for an event has nothing to wait for here.
        unsafe {
            vbl.request = Request {
                kind: RELATIVE | EVENT,
                sequence: 1,
                signal: 0,
            };
            assert_eq!(drmWaitVBlank(-1, &mut vbl), -1);
        }
        assert_eq!(unsafe { drmWaitVBlank(-1, std::ptr::null_mut()) }, -1);
    }
}
