//! The default window, for `VK_NN_vi_surface`, and the display's vsync event. Externs from
//! `display/native_window.h`, `services/vi.h` and `kernel/event.h`, libnx 4.12.0. Referencing
//! `nwindowGetDefault` links libnx's default-window init, which opens the display and a layer
//! before `main`.

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

    use super::{Event, ViDisplay};

    extern "C" {
        pub fn nwindowGetDefault() -> *mut c_void;
        pub fn nwindowGetDimensions(nw: *mut c_void, out_width: *mut u32, out_height: *mut u32) -> u32;
        pub fn nwindowSetDimensions(nw: *mut c_void, width: u32, height: u32) -> u32;
        pub fn viOpenDisplay(name: *const c_char, display: *mut ViDisplay) -> u32;
        pub fn viGetDisplayVsyncEvent(display: *mut ViDisplay, event_out: *mut Event) -> u32;
        pub fn eventWait(t: *mut Event, timeout: u64) -> u32;
        pub fn eventClear(t: *mut Event) -> u32;
    }
}

/// The default `NWindow*`, alive for the whole process.
#[cfg(target_os = "horizon")]
pub fn default_window() -> *mut c_void {
    // SAFETY: no arguments; returns a pointer to libnx's static window.
    unsafe { ffi::nwindowGetDefault() }
}

/// The window's size, or `(0, 0)` for any pointer but [`default_window`]'s.
#[cfg(target_os = "horizon")]
pub fn window_size(w: *mut c_void) -> (u32, u32) {
    let win = default_window();
    if w != win {
        return (0, 0);
    }
    let (mut width, mut height) = (0, 0);
    // SAFETY: `win` is libnx's static default window; the out-pointers are locals.
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
    // SAFETY: `win` is libnx's static default window; libnx checks the window's buffer state.
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
    // SAFETY: the default window is libnx's static `NWindow` (128 bytes in 4.12.0); the flag is
    // a byte that `nwindowQueueBuffer` writes on this same thread, inside the present.
    unsafe { default_window().cast::<u8>().add(NWINDOW_BEHIND).read() != 0 }
}

/// The default display's vsync event: vi hands it out once per display.
#[cfg(target_os = "horizon")]
static VSYNC: OnceLock<Event> = OnceLock::new();

/// Opens the default display a second time (the window's own is private to libnx) and takes its
/// vsync event for the process. A second call does nothing.
#[cfg(target_os = "horizon")]
pub fn open_vsync() -> anyhow::Result<()> {
    if VSYNC.get().is_some() {
        return Ok(());
    }
    let mut display = ViDisplay {
        display_id: 0,
        display_name: [0; 0x40],
        initialized: false,
    };
    // SAFETY: the name is NUL-terminated and `display` is a local libnx fills in; vi is up,
    // initialised by the default window before `main`.
    let rc = unsafe { ffi::viOpenDisplay(c"Default".as_ptr(), &mut display) };
    if rc != 0 {
        return Err(crate::nx_error("open the default display", rc));
    }
    let mut event = Event::default();
    // SAFETY: `display` was opened above and stays open for the process; `event` is a local.
    let rc = unsafe { ffi::viGetDisplayVsyncEvent(&mut display, &mut event) };
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
    /// `Event` 12 (align 4).
    #[test]
    fn layouts_match_libnx() {
        assert_eq!((size_of::<ViDisplay>(), align_of::<ViDisplay>()), (80, 8));
        assert_eq!(offset_of!(ViDisplay, initialized), 72);
        assert_eq!((size_of::<Event>(), align_of::<Event>()), (12, 4));
        assert_eq!(offset_of!(Event, autoclear), 8);
    }
}
