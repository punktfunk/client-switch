//! The default window, for `VK_NN_vi_surface`. Externs from `display/native_window.h`, libnx
//! 4.12.0. Referencing `nwindowGetDefault` links libnx's default-window init, which opens the
//! display and a layer before `main`.

#[cfg(target_os = "horizon")]
use core::ffi::c_void;

#[cfg(target_os = "horizon")]
mod ffi {
    use core::ffi::c_void;

    extern "C" {
        pub fn nwindowGetDefault() -> *mut c_void;
        pub fn nwindowGetDimensions(nw: *mut c_void, out_width: *mut u32, out_height: *mut u32) -> u32;
        pub fn nwindowSetDimensions(nw: *mut c_void, width: u32, height: u32) -> u32;
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
