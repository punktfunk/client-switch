//! The title's life: start-up, then the menu and the stream in turn, then the one exit.
//!
//! [`run`] never returns. The console records a title that returns from `main` as a crash, so
//! every way out goes through [`crate::platform::Lifecycle::exit`] after the logs are flushed.

mod connect;
mod hud;
mod menu;
mod motion;
pub mod pads;
mod stream;
pub mod testrun;
mod touch;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use crate::core::model::bitrate_max_kbps;
use crate::platform::{ClockStage, Display, Lifecycle, Pads, Platform, SystemEvent};
use crate::services::power::ExitPlan;
use crate::services::store;
use crate::session::{self, Media};

/// The halves of the platform the loops share, owned by the main thread.
pub(crate) struct App {
    pub display: Box<dyn Display>,
    /// `None` while the stream's input thread holds them.
    pub pads: Option<Box<dyn Pads>>,
    pub lifecycle: Box<dyn Lifecycle>,
    pub media: Media,
    pub identity: (String, String),
    /// A scripted run from `test-run.json`, taken at start-up.
    pub test_run: Option<testrun::TestRun>,
    pub device: crate::platform::DeviceInfo,
}

impl App {
    /// System events since the last call. `Suspend` also sets [`SUSPENDED`].
    fn system_events(&mut self) -> Vec<SystemEvent> {
        let mut events = Vec::new();
        self.lifecycle.poll_events(&mut events);
        for event in &events {
            tracing::info!("system: {event:?}");
            match event {
                SystemEvent::Suspend => SUSPENDED.store(true, Ordering::Relaxed),
                SystemEvent::Resume => SUSPENDED.store(false, Ordering::Relaxed),
                SystemEvent::FocusLost | SystemEvent::FocusGained | SystemEvent::OperationMode(_) => {}
            }
        }
        events
    }

    /// `" | clocks gpu … MHz"` for a 10 s summary line, empty where the clocks cannot be read.
    fn clocks_suffix(&self) -> String {
        self.lifecycle
            .clocks()
            .map_or_else(String::new, |c| format!(" | clocks {c}"))
    }
}

/// Sleep is on its way: no new session may start.
static SUSPENDED: AtomicBool = AtomicBool::new(false);

/// How one pass through the menu ended.
enum UiOutcome {
    Launch(Box<connect::ConnectOutcome>),
    /// Leave the title, with the selected host's exit action unfired.
    Quit(Option<ExitPlan>),
}

enum StreamOutcome {
    /// Back to the menu, with why if the stream did not end on the player's word.
    ReturnToMenu(Option<String>),
    Quit(Option<ExitPlan>),
}

/// The startup link probe's burst: the connection test's proven-safe target.
const ABR_PROBE_KBPS: u32 = 320_000;

/// The two automatic-bitrate knobs punktfunk-core reads from the environment. Set before any
/// thread exists: `set_var` is not thread-safe.
fn set_abr_env(pyrowave: bool) {
    std::env::set_var("PUNKTFUNK_ABR_PROBE_KBPS", ABR_PROBE_KBPS.to_string());
    std::env::set_var(
        "PUNKTFUNK_ABR_MAX_MBPS",
        (bitrate_max_kbps(pyrowave) / 1_000).to_string(),
    );
}

/// Runs the title until the player leaves it. Never returns.
pub fn run(platform: Platform) -> ! {
    set_abr_env(platform.video.caps().pyrowave);
    let Platform {
        display,
        video,
        audio,
        pads,
        storage,
        mut lifecycle,
        log,
        device,
    } = platform;
    crate::services::paths::install(storage.data_dir());
    // Without a log there is nothing to diagnose a failure from; the title still runs.
    let flush = crate::logger::init_subscriber(&crate::services::paths::logs_dir(), log).ok();
    if let Some(release) = lifecycle.clock_release() {
        let _ = CLOCK_RELEASE.set(release);
    }
    install_panic_hook();
    tracing::info!(
        "punktfunk-switch {} on {} ({} firmware {}), gpu {}, ui {}{}",
        crate::core::build_id(),
        device.platform,
        device.model,
        device.firmware,
        device.gpu,
        device.ui,
        if device.applet_mode { ", applet mode" } else { "" },
    );
    punktfunk_core::client::set_client_label(&client_label(&device));
    lifecycle.set_clock_stage(ClockStage::Menu);
    crate::core::caps::install(video.caps());
    // ureq's agents resolve their provider from the process default and panic without one.
    punktfunk_core::tls::install_default_provider();
    let mut app = App {
        display,
        pads: Some(pads),
        lifecycle,
        media: Media { video, audio },
        identity: (String::new(), String::new()),
        test_run: testrun::take(),
        device,
    };
    let plan = match store::load_or_create_identity() {
        Ok(identity) => {
            app.identity = identity;
            app.lifecycle.hide_splash();
            alternate(&mut app)
        }
        Err(e) => {
            tracing::error!("client identity: {e:#}");
            None
        }
    };
    // A connect abandoned mid-handshake holds a session the host would refuse the action over.
    connect::await_abandoned();
    if let Some(plan) = plan {
        plan.run();
    }
    tracing::info!("exiting");
    // The console's GPU memory lives in the title's heap: a device left open keeps those pages
    // mapped, and hbl faults loading the next NRO over them. `exit` never returns to drop them.
    let App {
        display,
        pads,
        lifecycle,
        media,
        ..
    } = app;
    drop((display, pads, media));
    drop(flush);
    lifecycle.exit()
}

/// What the first menu says when the title runs as an applet (plan D13).
const APPLET_MODE_NOTICE: &str =
    "Opened from the Album, punktfunk has little memory and may close — hold R while starting a game to open it with full memory.";

/// The menu and the stream in turn, until one of them quits.
fn alternate(app: &mut App) -> Option<ExitPlan> {
    let mut notice = app.device.applet_mode.then(|| APPLET_MODE_NOTICE.to_string());
    loop {
        let outcome = match menu::run(app, notice.take()) {
            Ok(outcome) => outcome,
            Err(e) => {
                tracing::error!("menu: {e:#}");
                return None;
            }
        };
        match outcome {
            UiOutcome::Quit(plan) => return plan,
            UiOutcome::Launch(launch) => {
                app.lifecycle.set_clock_stage(ClockStage::Stream);
                match stream::run(app, *launch) {
                    StreamOutcome::ReturnToMenu(why) => {
                        app.lifecycle.set_clock_stage(ClockStage::Menu);
                        notice = why;
                    }
                    StreamOutcome::Quit(plan) => return plan,
                }
            }
        }
    }
}

/// What the host's device list calls this console: "Switch" and its firmware.
fn client_label(device: &crate::platform::DeviceInfo) -> String {
    format!("Switch {}", device.firmware).trim_end().to_string()
}

/// The platform's way to hand the clocks back, for the panic hook.
static CLOCK_RELEASE: OnceLock<fn()> = OnceLock::new();

/// A panic on any thread goes to the log, then its frames where the target can walk them, then
/// the process aborts. The message is flushed and the clocks go back before the walk: on the
/// console, which builds without unwind tables, the walk itself aborts.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed");
        tracing::error!("PANIC on thread {name:?}: {info}");
        crate::logger::flush();
        if let Some(release) = CLOCK_RELEASE.get() {
            release();
        }
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!("PANIC on thread {name:?}, frames:\n{backtrace}");
        crate::logger::flush();
    }));
}

/// Ends a session nothing will stream, in the order the host expects.
fn teardown(connected: session::Connected) {
    connected.disconnect_quit();
    connected.shutdown_and_quit();
}
