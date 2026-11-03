//! The title's life on the applet service: focus, sleep and dock changes as [`SystemEvent`]s,
//! and the way out.
//!
//! The exit lock is held from the start, so HOME's close reaches [`Lifecycle::poll_events`]:
//! the session gets one `Suspend` to end cleanly, and the next poll leaves.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use pf_nx_app::platform::{Clocks, Lifecycle, SocketCalls, SystemEvent};
use pf_nx_sys::applet::{self, FocusState, HookEvent};
use pf_nx_sys::{clk, mem, rt, socket, swkbd};

pub struct NxLifecycle {
    events: mpsc::Receiver<HookEvent>,
    _hook: applet::Hook,
    /// The system asked the title to close; the next poll leaves.
    closing: bool,
    /// The first poll logs the applet type and mode: the logger starts after `new`.
    reported: bool,
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
            reported: false,
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
        if !std::mem::replace(&mut self.reported, true) {
            tracing::info!(
                "lifecycle: {:?}, {:?}, clocks {}",
                applet::applet_type(),
                applet::operation_mode(),
                self.clocks()
                    .map_or_else(|| "unreadable".to_string(), |c| c.to_string())
            );
        }
        // `appletMainLoop` in its halves, so every message is logged: no hook event and no
        // operation-mode change has reached a log yet, the dock change included.
        if let Some(msg) = applet::take_message() {
            tracing::info!("lifecycle: applet message {msg} ({})", applet::message_name(msg));
            if !applet::process_message(msg) {
                tracing::info!("lifecycle: the system asked the title to close");
                self.closing = true;
                out.push(SystemEvent::Suspend);
                return;
            }
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
                // The system moved the title to another clock configuration (WP5's question).
                HookEvent::PerformanceMode => tracing::info!(
                    "lifecycle: performance mode changed, clocks {}",
                    self.clocks()
                        .map_or_else(|| "unreadable".to_string(), |c| c.to_string())
                ),
                HookEvent::Exit => {}
            }
        }
    }

    fn clocks(&self) -> Option<Clocks> {
        clk::clocks().map(|c| Clocks {
            cpu_hz: c.cpu_hz,
            gpu_hz: c.gpu_hz,
            emc_hz: c.emc_hz,
        })
    }

    fn socket_calls(&self) -> Option<SocketCalls> {
        let c = rt::socket_calls();
        let (lean_waker, read_wait_us) = super::socket_switches();
        Some(SocketCalls {
            polls: c.polls,
            batch_reads: c.batch_reads,
            batch_datagrams: c.batch_datagrams,
            batch_read_ns: c.batch_read_ns,
            recvs: c.recvs,
            recv_froms: c.recv_froms,
            sends: c.sends,
            send_tos: c.send_tos,
            lean_waker,
            read_wait_us,
        })
    }

    fn keyboard(&mut self) -> Option<String> {
        match swkbd::ask("Text to type on the PC", "Send") {
            Ok(text) => Some(text),
            Err(rc) => {
                tracing::info!("keyboard: closed without text ({})", pf_nx_sys::result_code(rc));
                None
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
