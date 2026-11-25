//! The title's life on the applet service: focus, sleep and dock changes as [`SystemEvent`]s,
//! and the way out.
//!
//! The exit lock is held from the start, so HOME's close reaches [`Lifecycle::poll_events`]:
//! the session gets one `Suspend` to end cleanly, and the next poll leaves.

use std::cell::OnceCell;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use pf_nx_app::platform::{Clocks, Lifecycle, SocketCalls, SystemEvent};
use pf_nx_sys::applet::{self, FocusState, HookEvent};
use pf_nx_sys::{clk, mem, pl, rt, set, socket, swkbd};

pub struct NxLifecycle {
    events: mpsc::Receiver<HookEvent>,
    _hook: applet::Hook,
    /// The system asked the title to close; the next poll leaves.
    closing: bool,
    /// The first poll logs the applet type and mode: the logger starts after `new`.
    reported: bool,
    /// The shared fonts, read at the first ask.
    fonts: OnceCell<Vec<&'static [u8]>>,
    /// The last poll: a long gap since is a sleep or a HOME the title was suspended through.
    last_poll: Option<Instant>,
    /// The last time the system was asked for the operation mode.
    last_ask: Option<Instant>,
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
            fonts: OnceCell::new(),
            last_poll: None,
            last_ask: None,
        }
    }

    /// Asks the system for the operation mode and follows it where it differs from the title's.
    /// libnx refreshes its copy only on `OperationModeChanged`, and a dock change during sleep
    /// sends none (2026-11-24: docked asleep, woke at 720p). `why` names a wake and logs the
    /// answer either way; the routine ask logs only a difference.
    fn ask_mode(&mut self, why: Option<&str>, out: &mut Vec<SystemEvent>) {
        self.last_ask = Some(Instant::now());
        let title = super::operation_mode();
        let system = match applet::system_operation_mode() {
            Ok(mode) => super::from_applet(mode),
            Err(rc) => {
                if let Some(why) = why {
                    tracing::warn!(
                        "lifecycle: {why}: the operation mode was not read ({})",
                        pf_nx_sys::result_code(rc)
                    );
                }
                return;
            }
        };
        if let Some(why) = why {
            tracing::info!(
                "lifecycle: {why}: the system says {system:?}, libnx's copy {:?}, the title {title:?}",
                applet::operation_mode()
            );
        }
        if system != title {
            tracing::info!("lifecycle: the system is {system:?} and no change was sent: following it");
            super::set_operation_mode(system);
            out.push(SystemEvent::OperationMode(system));
        }
    }
}

/// A poll this long after the last one means the title was suspended (sleep, or HOME).
const SUSPENDED_GAP: Duration = Duration::from_secs(1);
/// How often the system is asked for the operation mode with nothing else to prompt it.
const ASK_EVERY: Duration = Duration::from_secs(2);

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
        // `appletMainLoop` in its halves, so every message is logged. A dock change while awake
        // arrives as `OperationModeChanged`; one during sleep sent none (2026-11-24).
        if let Some(msg) = applet::take_message() {
            tracing::info!("lifecycle: applet message {msg} ({})", applet::message_name(msg));
            if !applet::process_message(msg) {
                tracing::info!("lifecycle: the system asked the title to close");
                self.closing = true;
                out.push(SystemEvent::Suspend);
                return;
            }
        }
        let now = Instant::now();
        let mut wake = self
            .last_poll
            .replace(now)
            .filter(|then| now.duration_since(*then) >= SUSPENDED_GAP)
            .map(|then| format!("back after {:.1} s away", now.duration_since(then).as_secs_f64()));
        for event in self.events.try_iter() {
            match event {
                HookEvent::FocusState => out.push(match applet::focus_state() {
                    FocusState::InFocus => {
                        wake.get_or_insert_with(|| "back in focus".into());
                        SystemEvent::FocusGained
                    }
                    FocusState::OutOfFocus => SystemEvent::FocusLost,
                    FocusState::Background => SystemEvent::Suspend,
                }),
                HookEvent::Resume => {
                    wake.get_or_insert_with(|| "resumed".into());
                    out.push(SystemEvent::Resume);
                }
                // libnx refreshed its copy with this message: the title follows it.
                HookEvent::OperationMode => {
                    let mode = super::from_applet(applet::operation_mode());
                    super::set_operation_mode(mode);
                    out.push(SystemEvent::OperationMode(mode));
                }
                // The system moved the title to another clock configuration (WP5's question).
                HookEvent::PerformanceMode => tracing::info!(
                    "lifecycle: performance mode changed, clocks {}",
                    self.clocks()
                        .map_or_else(|| "unreadable".to_string(), |c| c.to_string())
                ),
                HookEvent::Exit => {}
            }
        }
        if wake.is_some() || self.last_ask.is_none_or(|then| now.duration_since(then) >= ASK_EVERY) {
            self.ask_mode(wake.as_deref(), out);
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
        Some(SocketCalls {
            polls: c.polls,
            batch_reads: c.batch_reads,
            batch_datagrams: c.batch_datagrams,
            batch_read_ns: c.batch_read_ns,
            recvs: c.recvs,
            recv_froms: c.recv_froms,
            sends: c.sends,
            send_tos: c.send_tos,
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

    /// pl:u's faces, ordered for the system's language. The console kit copies each face.
    fn fallback_fonts(&self) -> &[&'static [u8]] {
        self.fonts.get_or_init(|| {
            let language = set::system_languages().into_iter().next().unwrap_or_default();
            let faces = pl::shared_fonts(&language);
            let bytes: usize = faces.iter().map(|f| f.len()).sum();
            tracing::info!(
                "fonts: {} shared faces, {:.1} MiB, for {language:?}",
                faces.len(),
                bytes as f64 / MIB
            );
            faces
        })
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
