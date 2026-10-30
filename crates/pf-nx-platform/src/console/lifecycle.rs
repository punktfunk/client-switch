//! The title's life on the applet service: focus, sleep and dock changes as [`SystemEvent`]s,
//! and the way out.
//!
//! The exit lock is held from the start, so HOME's close reaches [`Lifecycle::poll_events`]:
//! the session gets one `Suspend` to end cleanly, and the next poll leaves.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use pf_nx_app::platform::{Lifecycle, SystemEvent};
use pf_nx_sys::applet::{self, FocusState, HookEvent};
use pf_nx_sys::{mem, rt, socket};

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

    /// Returns the socket driver's lent heap, closes the detached threads' stacks, and leaves
    /// through libnx's `__appExit`: back to hbl when the heap is clean, else through AM to HOME.
    fn exit(&self) -> ! {
        applet::unlock_exit();
        socket::exit();
        release_heap();
        std::process::exit(0)
    }
}

/// How long the exit waits for detached threads to end, and again for services to unmap their
/// transfer memory (`socketExit` does not wait for it).
const RELEASE_WAIT: Duration = Duration::from_secs(1);
const MIB: f64 = 1_048_576.0;

/// hbl reads the next NRO into this heap in one read and faults (2001-0106) on any page still
/// locked: a thread stack, transfer memory, an nvmap object. Closes what the title can, then
/// exits through AM if anything is still held, so the worst case is HOME instead of a crash.
/// The logger is gone by now: the report goes to `exit.log` beside the title and to nxlink.
fn release_heap() {
    let mut report = String::new();
    match rt::close_detached_threads(RELEASE_WAIT) {
        Some(t) => {
            report += &format!(
                "detached threads: closed {}, {} still running, {} close failures and {} untracked since launch\n",
                t.closed, t.running, t.close_failed, t.untracked
            )
        }
        None => report += "detached threads: another thread held the closer\n",
    }
    let started = Instant::now();
    let held = loop {
        match mem::held_heap() {
            Ok(held) if !held.is_empty() && started.elapsed() < RELEASE_WAIT => {
                std::thread::sleep(Duration::from_millis(1));
            }
            other => break other,
        }
    };
    match held {
        Ok(held) if held.is_empty() => {
            report += &format!("heap: clean after {} ms; back to hbl\n", started.elapsed().as_millis())
        }
        Ok(held) => {
            let bytes: u64 = held.iter().map(|b| b.end - b.start).sum();
            report += &format!(
                "heap: {} blocks, {:.2} MiB, still held after {} ms; exiting through AM to HOME\n",
                held.len(),
                bytes as f64 / MIB,
                RELEASE_WAIT.as_millis()
            );
            for b in held.iter().take(16) {
                let why = match b.attr {
                    a if a & mem::ATTRS_NVMAP != 0 => " (nvmap: GPU memory)",
                    a if a & mem::ATTR_LOCKED != 0 => " (locked: a thread stack or transfer memory)",
                    _ => "",
                };
                report += &format!(
                    "  {:#x}-{:#x} {:.2} MiB type {:#x} perm {:#x} attr {:#x}{why}\n",
                    b.start,
                    b.end,
                    (b.end - b.start) as f64 / MIB,
                    b.kind,
                    b.perm,
                    b.attr
                );
            }
            mem::exit_through_am();
        }
        Err((addr, rc)) => {
            report += &format!(
                "heap: query at {addr:#x} failed ({}); exiting through AM to HOME\n",
                pf_nx_sys::result_code(rc)
            );
            mem::exit_through_am();
        }
    }
    print!("{report}");
    let _ = std::fs::write(format!("{}/exit.log", super::DATA_DIR), report);
}
