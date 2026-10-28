//! The probe on the console: the socket driver with 4 MiB UDP buffers, stdout to nxlink when a
//! host is listening and to the screen otherwise.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use pf_nx_sys::{applet, console, socket};

/// Set once the screen console holds stdout.
static SCREEN: AtomicBool = AtomicBool::new(false);

/// A 4K burst at the host's pacing fits in this; libnx's default is 0xA500.
const UDP_BUF: u32 = 4 << 20;

/// Starts the socket driver with 4 MiB UDP buffers, else with libnx's defaults. Says which took.
pub fn sockets_up() -> String {
    let config = socket::Config {
        udp_rx_buf: UDP_BUF,
        udp_tx_buf: UDP_BUF,
        ..socket::Config::default()
    };
    match socket::init(&config) {
        Ok(()) => format!("ok udp_buf={UDP_BUF}"),
        Err(e) => match socket::init(&socket::Config::default()) {
            Ok(()) => format!("fail udp_buf={UDP_BUF} {e:#}, defaults ok"),
            Err(e2) => format!("fail-both udp_buf={UDP_BUF} {e:#}, defaults {e2:#}"),
        },
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
