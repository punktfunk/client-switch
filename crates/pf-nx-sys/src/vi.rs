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
