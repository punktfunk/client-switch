//! libnx's text console on the default framebuffer (`runtime/devices/console.h`, libnx 4.12.0):
//! stdout on the screen when no nxlink host is listening. All three take libnx's default console.

use core::ffi::c_void;
use core::ptr::null_mut;

extern "C" {
    fn consoleInit(con: *mut c_void) -> *mut c_void;
    fn consoleUpdate(con: *mut c_void);
    fn consoleExit(con: *mut c_void);
}

/// Points stdout at the screen.
pub fn init() {
    // SAFETY: null selects libnx's default console on the framebuffer.
    unsafe { consoleInit(null_mut()) };
}

/// Shows what was printed since the last call.
pub fn update() {
    // SAFETY: the default console, after `init`; libnx ignores an update before it.
    unsafe { consoleUpdate(null_mut()) };
}

pub fn exit() {
    // SAFETY: the default console, after `init`.
    unsafe { consoleExit(null_mut()) };
}
