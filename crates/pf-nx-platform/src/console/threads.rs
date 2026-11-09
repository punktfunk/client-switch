//! Thread priorities. Horizon runs the most urgent ready thread on a core, 0x00 first and with
//! no time slice under 0x3B; the main thread starts at 0x2C, every std thread at libnx's 0x3B.
//!
//! Core's hot threads (the socket reader, the frame pump, the runtime's workers) and the audio
//! writer run ahead of the main thread. The process's priority mask can refuse a value, so each
//! falls back once to the main thread's own, still ahead of every 0x3B thread. The outcome is
//! logged once per thread name.

use std::sync::{Mutex, PoisonError};

use pf_nx_sys::thread;

/// The main thread's priority, and the fallback.
const MAIN: i32 = 0x2C;
/// Core's receive and pump threads and the decode thread: a burst is read and decoded while
/// the main thread draws.
pub const HOT: i32 = 0x2B;
/// The audio writer wakes for one 5 ms buffer and blocks again, so it may come first.
pub const AUDIO: i32 = 0x2A;

/// Thread names whose outcome is logged.
static LOGGED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Core calls this at the start of each hot thread; the first install wins.
pub fn install() {
    punktfunk_core::client::set_thread_boost(|| raise(HOT));
}

/// Raises the calling thread to `prio`, else to [`MAIN`]. A thread both refuse stays where it is.
pub fn raise(prio: i32) {
    let (got, refused) = match thread::set_current_priority(prio) {
        Ok(()) => (Ok(prio), None),
        Err(e) => (
            thread::set_current_priority(MAIN).map(|()| MAIN),
            Some(format!("{prio:#x}: {e}")),
        ),
    };
    let name = std::thread::current().name().unwrap_or("unnamed").to_owned();
    {
        let mut logged = LOGGED.lock().unwrap_or_else(PoisonError::into_inner);
        if logged.contains(&name) {
            return;
        }
        logged.push(name.clone());
    }
    let refused = refused.as_deref();
    match got {
        Ok(p) => tracing::info!(
            thread = %name,
            priority = %format!("{p:#x}"),
            cores = %format!("{:#x}", thread::core_mask()),
            refused,
            "thread priority raised"
        ),
        Err(e) => tracing::warn!(thread = %name, error = %e, refused, "thread priority left at the default"),
    }
}
