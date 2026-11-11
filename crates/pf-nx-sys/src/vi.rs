//! The default window, for `VK_NN_vi_surface`, and the display's vsync event. Externs from
//! `display/native_window.h`, `services/vi.h` and `kernel/event.h`, libnx 4.12.0.
//!
//! vi opens a display once per session and libnx keeps its handle private, so this module
//! defines the default window itself: `nwindowGetDefault`, `__nx_win_init` and `__nx_win_exit`
//! here are every symbol of libnx's `default_window.c`, and the linker leaves that object out.
//! libnx's start-up calls `__nx_win_init` before `main`.

#[cfg(target_os = "horizon")]
use core::ffi::c_void;
#[cfg(target_os = "horizon")]
use std::sync::OnceLock;

/// `ViDisplay` (services/vi.h): what `viOpenDisplay` fills in.
#[repr(C)]
pub struct ViDisplay {
    pub display_id: u64,
    pub display_name: [u8; 0x40],
    pub initialized: bool,
}

/// `ViLayer` (services/vi.h): the layer's id, its binder object and two flag bits.
#[repr(C)]
pub struct ViLayer {
    pub layer_id: u64,
    pub igbp_binder_obj_id: u32,
    pub flags: u8,
}

/// `NWindow` (`display/native_window.h`), 128 bytes aligned to 8. Only libnx reads it, but for
/// [`NWINDOW_BEHIND`].
#[repr(C, align(8))]
pub struct NWindow(pub [u8; 128]);

/// `Event` (kernel/event.h): a kernel event's handles.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Event {
    pub revent: u32,
    pub wevent: u32,
    pub autoclear: bool,
}

/// `offsetof(NWindow, consumer_running_behind)`: `nwindowQueueBuffer` sets it when the queue
/// held more than the buffer just queued. Its accessor is `static inline`, so it is read here.
#[cfg(target_os = "horizon")]
const NWINDOW_BEHIND: usize = 126;

#[cfg(target_os = "horizon")]
mod ffi {
    use core::ffi::{c_char, c_void};

    use super::{Event, NWindow, ViDisplay, ViLayer};

    extern "C" {
        pub fn viInitialize(service_type: i32) -> u32;
        pub fn viExit();
        pub fn viCloseDisplay(display: *mut ViDisplay) -> u32;
        pub fn viCreateLayer(display: *const ViDisplay, layer: *mut ViLayer) -> u32;
        pub fn viSetLayerScalingMode(layer: *mut ViLayer, scaling_mode: u32) -> u32;
        pub fn viCloseLayer(layer: *mut ViLayer) -> u32;
        pub fn nwindowCreateFromLayer(nw: *mut NWindow, layer: *const ViLayer) -> u32;
        pub fn nwindowClose(nw: *mut NWindow);
        pub fn diagAbortWithResult(rc: u32) -> !;
        pub fn eventClose(t: *mut Event);
        pub fn nwindowGetDimensions(nw: *mut c_void, out_width: *mut u32, out_height: *mut u32) -> u32;
        pub fn nwindowSetDimensions(nw: *mut c_void, width: u32, height: u32) -> u32;
        pub fn viOpenDisplay(name: *const c_char, display: *mut ViDisplay) -> u32;
        pub fn viGetDisplayVsyncEvent(display: *mut ViDisplay, event_out: *mut Event) -> u32;
        pub fn eventWait(t: *mut Event, timeout: u64) -> u32;
        pub fn eventClear(t: *mut Event) -> u32;
    }
}

/// `ViServiceType_Default` (services/vi.h): the service the title's applet type allows.
#[cfg(target_os = "horizon")]
const VI_SERVICE_DEFAULT: i32 = -1;
/// `ViScalingMode_FitToLayer` (services/vi.h).
#[cfg(target_os = "horizon")]
const VI_SCALING_FIT_TO_LAYER: u32 = 2;

// The default display, its layer and the window on it. `__nx_win_init` fills them before
// `main` and `__nx_win_exit` closes them after it; between the two only libnx writes them.
#[cfg(target_os = "horizon")]
static mut DISPLAY: ViDisplay = ViDisplay {
    display_id: 0,
    display_name: [0; 0x40],
    initialized: false,
};
#[cfg(target_os = "horizon")]
static mut LAYER: ViLayer = ViLayer {
    layer_id: 0,
    igbp_binder_obj_id: 0,
    flags: 0,
};
#[cfg(target_os = "horizon")]
static mut WINDOW: NWindow = NWindow([0; 128]);

/// Keeps the two hooks in the link: libnx's start-up refers to them weakly, which pulls nothing.
#[cfg(target_os = "horizon")]
#[used]
static HOOKS: [unsafe extern "C" fn(); 2] = [__nx_win_init, __nx_win_exit];

/// libnx's name for [`default_window`], for C callers.
#[cfg(target_os = "horizon")]
#[no_mangle]
pub extern "C" fn nwindowGetDefault() -> *mut c_void {
    (&raw mut WINDOW).cast()
}

/// Opens the default display, a layer scaled to fit and the 1280x720 window on it, as libnx's
/// own hook does. Aborts the title with vi's result where a step fails.
///
/// # Safety
/// For libnx's start-up alone, once, before `main`.
#[cfg(target_os = "horizon")]
#[no_mangle]
pub unsafe extern "C" fn __nx_win_init() {
    // SAFETY: single-threaded start-up; the statics are zeroed and libnx fills each in turn,
    // every later call taking what the one before it opened. The name is NUL-terminated.
    unsafe {
        let mut rc = ffi::viInitialize(VI_SERVICE_DEFAULT);
        if rc == 0 {
            rc = ffi::viOpenDisplay(c"Default".as_ptr(), &raw mut DISPLAY);
        }
        if rc == 0 {
            rc = ffi::viCreateLayer(&raw const DISPLAY, &raw mut LAYER);
        }
        if rc == 0 {
            rc = ffi::viSetLayerScalingMode(&raw mut LAYER, VI_SCALING_FIT_TO_LAYER);
        }
        if rc == 0 {
            rc = ffi::nwindowCreateFromLayer(&raw mut WINDOW, &raw const LAYER);
        }
        if rc != 0 {
            ffi::diagAbortWithResult(rc);
        }
        ffi::nwindowSetDimensions((&raw mut WINDOW).cast(), 1280, 720);
    }
}

/// Closes what [`__nx_win_init`] opened, and the vsync event where one was taken.
///
/// # Safety
/// For libnx's shutdown alone, once, after every thread that presents has joined.
#[cfg(target_os = "horizon")]
#[no_mangle]
pub unsafe extern "C" fn __nx_win_exit() {
    // SAFETY: single-threaded shutdown; each object was opened by `__nx_win_init` or
    // `open_vsync`, and nothing uses the window or the event after this.
    unsafe {
        if let Some(event) = VSYNC.get() {
            ffi::eventClose(std::ptr::from_ref(event).cast_mut());
        }
        ffi::nwindowClose(&raw mut WINDOW);
        ffi::viCloseLayer(&raw mut LAYER);
        ffi::viCloseDisplay(&raw mut DISPLAY);
        ffi::viExit();
    }
}

/// The default `NWindow*`, alive for the whole process.
#[cfg(target_os = "horizon")]
#[inline(never)]
pub fn default_window() -> *mut c_void {
    nwindowGetDefault()
}

/// The window's size, or `(0, 0)` for any pointer but [`default_window`]'s.
#[cfg(target_os = "horizon")]
pub fn window_size(w: *mut c_void) -> (u32, u32) {
    let win = default_window();
    if w != win {
        return (0, 0);
    }
    let (mut width, mut height) = (0, 0);
    // SAFETY: `win` is the static default window; the out-pointers are locals.
    let rc = unsafe { ffi::nwindowGetDimensions(win, &mut width, &mut height) };
    if rc != 0 {
        return (0, 0);
    }
    (width, height)
}

/// Sizes the default window for the next buffers registered with it: its default is 1280x720,
/// and a swapchain on it takes the window's size. `false` for any pointer but
/// [`default_window`]'s, or when libnx refuses, as it does while buffers are still registered.
#[cfg(target_os = "horizon")]
pub fn set_window_size(w: *mut c_void, width: u32, height: u32) -> bool {
    let win = default_window();
    if w != win {
        return false;
    }
    let was = window_size(win);
    if was == (width, height) {
        return true;
    }
    // SAFETY: `win` is the static default window; libnx checks the window's buffer state.
    let rc = unsafe { ffi::nwindowSetDimensions(win, width, height) };
    if rc != 0 {
        tracing::warn!(result = %crate::result_code(rc), "vi: the window stays {was:?}, {width}x{height} refused");
        return false;
    }
    tracing::info!("vi: window {width}x{height}, was {was:?}");
    true
}

/// Whether the last buffer queued on the default window found another still waiting for the
/// panel. Call on the thread that presents.
#[cfg(target_os = "horizon")]
pub fn queue_behind() -> bool {
    // SAFETY: the default window is the static `NWindow`, 128 bytes; the flag is a byte that
    // `nwindowQueueBuffer` writes on this same thread, inside the present.
    unsafe { default_window().cast::<u8>().add(NWINDOW_BEHIND).read() != 0 }
}

/// The default display's vsync event: vi hands it out once per display.
#[cfg(target_os = "horizon")]
static VSYNC: OnceLock<Event> = OnceLock::new();

/// Takes the default display's vsync event for the process. A second call does nothing.
#[cfg(target_os = "horizon")]
pub fn open_vsync() -> anyhow::Result<()> {
    if VSYNC.get().is_some() {
        return Ok(());
    }
    let mut event = Event::default();
    // SAFETY: `__nx_win_init` opened `DISPLAY` before `main` and nothing writes it until exit;
    // libnx only reads it. `event` is a local.
    let rc = unsafe { ffi::viGetDisplayVsyncEvent(&raw mut DISPLAY, &mut event) };
    if rc != 0 {
        return Err(crate::nx_error("take the display's vsync event", rc));
    }
    let _ = VSYNC.set(event);
    Ok(())
}

/// Drops a vblank already signalled, so the next [`wait_vsync`] waits for one after this call.
#[cfg(target_os = "horizon")]
pub fn clear_vsync() {
    if let Some(event) = VSYNC.get() {
        // SAFETY: libnx reads the event's handles and writes nothing through the pointer.
        unsafe { ffi::eventClear(std::ptr::from_ref(event).cast_mut()) };
    }
}

/// Blocks until a vblank, at most `timeout_ns`. `false` on a timeout, or before [`open_vsync`].
#[cfg(target_os = "horizon")]
pub fn wait_vsync(timeout_ns: u64) -> bool {
    let Some(event) = VSYNC.get() else {
        return false;
    };
    // SAFETY: as in `clear_vsync`; the event is autoclear, so the wait also resets it.
    unsafe { ffi::eventWait(std::ptr::from_ref(event).cast_mut(), timeout_ns) == 0 }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use std::mem::{align_of, offset_of, size_of};

    use super::*;

    /// Sizes from the 4.12.0 headers through devkitA64's GCC: `ViDisplay` 80 (align 8),
    /// `ViLayer` 16 (align 8), `NWindow` 128 (align 8), `Event` 12 (align 4).
    #[test]
    fn layouts_match_libnx() {
        assert_eq!((size_of::<ViDisplay>(), align_of::<ViDisplay>()), (80, 8));
        assert_eq!(offset_of!(ViDisplay, initialized), 72);
        assert_eq!((size_of::<ViLayer>(), align_of::<ViLayer>()), (16, 8));
        assert_eq!(offset_of!(ViLayer, flags), 12);
        assert_eq!((size_of::<NWindow>(), align_of::<NWindow>()), (128, 8));
        assert_eq!((size_of::<Event>(), align_of::<Event>()), (12, 4));
        assert_eq!(offset_of!(Event, autoclear), 8);
    }
}
