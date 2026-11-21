//! The probe on the console: the socket driver on libnx's defaults, stdout to nxlink when a host
//! is listening and to the screen otherwise.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use pf_nx_sys::{applet, console, socket};

/// Set once the screen console holds stdout.
static SCREEN: AtomicBool = AtomicBool::new(false);

/// Starts the socket driver on libnx's defaults. The UDP sizes are every new socket's buffer
/// reservation, and 4 MiB there failed every socket with ENOBUFS on the console; check 2's
/// `rcvbuf` grows one socket instead and reports what the stack grants.
pub fn sockets_up() -> String {
    let config = socket::Config::default();
    match socket::init(&config) {
        Ok(()) => format!("ok udp_rx_buf={} udp_tx_buf={}", config.udp_rx_buf, config.udp_tx_buf),
        Err(e) => format!("fail {e:#}"),
    }
}

/// Sends stdout and stderr to nxlink when the probe was launched by `nxlink`, else to the screen.
pub fn stdio_up() {
    if socket::nxlink_stdio().is_some() {
        return;
    }
    console::init();
    SCREEN.store(true, Ordering::Relaxed);
}

/// Set once check 4's swapchain took the window the console draws on.
static PAUSED: AtomicBool = AtomicBool::new(false);

/// Shows what was printed since the last call, when the screen console holds stdout.
pub fn screen_flush() {
    if SCREEN.load(Ordering::Relaxed) && !PAUSED.load(Ordering::Relaxed) {
        console::update();
    }
}

/// Closes the screen console for good, which lets go of the default window's framebuffer, so a
/// swapchain can take the window. Nothing prints to the screen after it. A console opened again
/// after the swapchain printed some 40 lines, then libnx aborted in `framebufferBegin` with
/// `LibnxError_BadGfxDequeueBuffer` (2345-0021).
#[cfg(feature = "vulkan")]
pub fn screen_pause() {
    if SCREEN.load(Ordering::Relaxed) {
        PAUSED.store(true, Ordering::Relaxed);
        console::exit();
    }
}

/// The screen console was closed for check 4's swapchain: lines go to the log file only.
pub fn screen_paused() -> bool {
    PAUSED.load(Ordering::Relaxed)
}

/// Keeps the screen's lines up until HOME closes the probe, then tears down. After check 4 the
/// console is closed and the screen holds what the swapchain left.
pub fn finish() {
    if SCREEN.load(Ordering::Relaxed) {
        let console = !screen_paused();
        while applet::main_loop() {
            if console {
                console::update();
            }
            std::thread::sleep(Duration::from_micros(16_667));
        }
        if console {
            console::exit();
        }
    }
    socket::exit();
}

/// Tears down at once, for check 7: its last lines were read while it ran.
pub fn leave() {
    if SCREEN.load(Ordering::Relaxed) {
        console::exit();
    }
    socket::exit();
}
