//! The title's life on the applet service: focus, sleep and dock changes as [`SystemEvent`]s,
//! and the way out.
//!
//! The exit lock is held from the start, so HOME's close reaches [`Lifecycle::poll_events`]:
//! the session gets one `Suspend` to end cleanly, and the next poll leaves.

use std::sync::mpsc;

use pf_nx_app::platform::{Lifecycle, SystemEvent};
use pf_nx_sys::applet::{self, FocusState, HookEvent};
use pf_nx_sys::socket;

pub struct NxLifecycle {
    events: mpsc::Receiver<HookEvent>,
    _hook: applet::Hook,
    /// The system asked the title to close; the next poll leaves.
    closing: bool,
}

impl NxLifecycle {
    /// Hooks the applet's status changes. Call on the main thread, which pumps them.
    pub fn new() -> Self {
        let (tx, events) = mpsc::channel();
        let hook = applet::hook(move |e| {
            let _ = tx.send(e);
        });
        applet::lock_exit();
        Self {
            events,
            _hook: hook,
            closing: false,
        }
    }
}

impl Lifecycle for NxLifecycle {
    /// libnx shows no splash of its own.
    fn hide_splash(&self) {}

    fn poll_events(&mut self, out: &mut Vec<SystemEvent>) {
        if self.closing {
            self.exit();
        }
        if !applet::main_loop() {
            tracing::info!("lifecycle: the system asked the title to close");
            self.closing = true;
            out.push(SystemEvent::Suspend);
            return;
        }
        for event in self.events.try_iter() {
            match event {
                HookEvent::FocusState => out.push(match applet::focus_state() {
                    FocusState::InFocus => SystemEvent::FocusGained,
                    FocusState::OutOfFocus => SystemEvent::FocusLost,
                    FocusState::Background => SystemEvent::Suspend,
                }),
                HookEvent::Resume => out.push(SystemEvent::Resume),
                HookEvent::OperationMode => out.push(SystemEvent::OperationMode(super::operation_mode())),
                HookEvent::Exit | HookEvent::PerformanceMode => {}
            }
        }
    }

    /// Returns the socket driver's lent heap, then leaves through libnx's `__appExit`.
    // TODO(verify): threads still running at exit keep their stacks locked, which crashes a
    // loader that reads the next NRO into the whole heap (sphaira); exiting through AM to HOME
    // (`__nx_applet_exit_mode = 1`) avoids that and needs a sys entry point.
    fn exit(&self) -> ! {
        applet::unlock_exit();
        socket::exit();
        std::process::exit(0)
    }
}
