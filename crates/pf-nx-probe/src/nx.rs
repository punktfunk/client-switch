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

/// Shows what was printed since the last call, when the screen console holds stdout.
pub fn screen_flush() {
    if SCREEN.load(Ordering::Relaxed) {
        console::update();
    }
}

/// Keeps the screen's lines up until HOME closes the probe, then tears down.
pub fn finish() {
    if SCREEN.load(Ordering::Relaxed) {
        while applet::main_loop() {
            console::update();
            std::thread::sleep(Duration::from_micros(16_667));
        }
        console::exit();
    }
    socket::exit();
}
