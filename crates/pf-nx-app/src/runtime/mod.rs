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
use std::time::Instant;

use crate::core::model::bitrate_max_kbps;
use crate::platform::{Clocks, Display, Lifecycle, Pads, Platform, SocketCalls, SystemEvent};
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

    /// One warning each time the CPU drops below stock. Read only: this client never sets a
    /// clock.
    fn warn_slow_cpu(&self, slow: &mut SlowCpu) {
        if let Some(cpu_mhz) = slow.drop_mhz(self.lifecycle.clocks()) {
            tracing::warn!(cpu_mhz, stock_mhz = STOCK_CPU_MHZ, "cpu clock below stock");
        }
    }

    /// The socket calls since the last call, one summary; the first call only starts the count.
    fn socket_window(&self, last: &mut Option<(SocketCalls, Instant)>) -> Option<String> {
        let now = self.lifecycle.socket_calls()?;
        let at = Instant::now();
        let window = last
            .as_ref()
            .map(|(before, then)| now.window(before, at.duration_since(*then).as_secs_f64()));
        *last = Some((now, at));
        window
    }
}

/// The CPU clock of every performance configuration a retail console runs a game at.
const STOCK_CPU_MHZ: u32 = 1020;

/// The CPU clock in whole MHz, when it reads below stock.
fn cpu_below_stock(clocks: Clocks) -> Option<u32> {
    let mhz = clocks.cpu_hz.saturating_add(500_000) / 1_000_000;
    (mhz < STOCK_CPU_MHZ).then_some(mhz)
}

/// Whether the last CPU reading was below stock, so each drop warns once.
#[derive(Default)]
struct SlowCpu(bool);

impl SlowCpu {
    /// The CPU clock in MHz to warn about: a reading below stock after one that was not. An
    /// unreadable clock changes nothing.
    fn drop_mhz(&mut self, clocks: Option<Clocks>) -> Option<u32> {
        let mhz = cpu_below_stock(clocks?);
        let was = std::mem::replace(&mut self.0, mhz.is_some());
        mhz.filter(|_| !was)
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

/// A file of this name in the data directory holds megabits a second: automatic bitrate's
/// ceiling for this run, for the handheld A/B. Outside [`CEILING_MBPS`] it is ignored.
const CEILING_MARKER: &str = "teto-mbps";
const CEILING_MBPS: std::ops::RangeInclusive<u32> = 5..=20;

/// The ceiling marker's megabits a second, when its content is a whole number in range.
fn marker_mbps(content: &str) -> Option<u32> {
    content.trim().parse().ok().filter(|m| CEILING_MBPS.contains(m))
}

/// The two automatic-bitrate knobs punktfunk-core reads from the environment. Set before any
/// thread exists: `set_var` is not thread-safe.
fn set_abr_env(max_mbps: u32) {
    std::env::set_var("PUNKTFUNK_ABR_PROBE_KBPS", ABR_PROBE_KBPS.to_string());
    std::env::set_var("PUNKTFUNK_ABR_MAX_MBPS", max_mbps.to_string());
}

/// Runs the title until the player leaves it. Never returns.
pub fn run(platform: Platform) -> ! {
    crate::services::paths::install(platform.storage.data_dir());
    let marker = std::fs::read_to_string(crate::services::paths::app_dir().join(CEILING_MARKER)).ok();
    let pinned = marker.as_deref().and_then(marker_mbps);
    let max_mbps = pinned.unwrap_or(bitrate_max_kbps(platform.video.caps().pyrowave) / 1_000);
    set_abr_env(max_mbps);
    let Platform {
        display,
        video,
        audio,
        pads,
        storage: _,
        lifecycle,
        log,
        device,
    } = platform;
    // Without a log there is nothing to diagnose a failure from; the title still runs.
    let flush = crate::logger::init_subscriber(&crate::services::paths::logs_dir(), log).ok();
    install_panic_hook();
    let language = pf_l10n::set_languages(&device.languages);
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
    tracing::info!("language {language} for the system's {:?}", device.languages);
    match (pinned, marker) {
        (None, Some(content)) => tracing::warn!(
            marker = CEILING_MARKER,
            content = content.trim(),
            accepts = "whole Mb/s from 5 to 20",
            max_mbps,
            "bitrate ceiling marker ignored"
        ),
        (pinned, _) => tracing::info!(max_mbps, from_marker = pinned.is_some(), "automatic bitrate ceiling"),
    }
    punktfunk_core::client::set_client_label(&client_label(&device));
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

/// What the first menu says when the title runs as an applet (plan D13). Holding `R` at
/// launch starts it with full memory.
fn applet_mode_notice() -> String {
    pf_l10n::console::console_switch_applet_mode("R")
}

/// The menu and the stream in turn, until one of them quits.
fn alternate(app: &mut App) -> Option<ExitPlan> {
    let mut notice = app.device.applet_mode.then(applet_mode_notice);
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
            UiOutcome::Launch(launch) => match stream::run(app, *launch) {
                StreamOutcome::ReturnToMenu(why) => notice = why,
                StreamOutcome::Quit(plan) => return plan,
            },
        }
    }
}

/// What the host's device list calls this console: "Switch" and its firmware.
fn client_label(device: &crate::platform::DeviceInfo) -> String {
    format!("Switch {}", device.firmware).trim_end().to_string()
}

/// A panic on any thread goes to the log, then its frames where the target can walk them, then
/// the process aborts. The message is flushed before the walk: on the console, which builds
/// without unwind tables, the walk itself aborts.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed");
        tracing::error!("PANIC on thread {name:?}: {info}");
        crate::logger::flush();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ceiling_marker_takes_whole_megabits_from_5_to_20() {
        assert_eq!(marker_mbps("12\n"), Some(12));
        assert_eq!(marker_mbps(" 5 "), Some(5));
        assert_eq!(marker_mbps("20"), Some(20));
        for ignored in ["4", "21", "", "12.5", "12 Mb/s", "-12"] {
            assert_eq!(marker_mbps(ignored), None, "{ignored:?}");
        }
    }

    #[test]
    fn only_a_cpu_below_stock_is_named() {
        let cpu = |hz| cpu_below_stock(cpu_at(hz));
        assert_eq!(cpu(612_000_000), Some(612));
        assert_eq!(cpu(1_019_999_999), None, "rounds to stock");
        assert_eq!(cpu(1_020_000_000), None);
        assert_eq!(cpu(1_785_000_000), None);
    }

    fn cpu_at(hz: u32) -> Clocks {
        Clocks {
            cpu_hz: hz,
            ..Clocks::default()
        }
    }

    #[test]
    fn each_drop_below_stock_warns_once() {
        let mut slow = SlowCpu::default();
        let mut read = |hz| slow.drop_mhz(Some(cpu_at(hz)));
        assert_eq!(read(612_000_000), Some(612), "below");
        assert_eq!(read(714_000_000), None, "still below");
        assert_eq!(read(1_020_000_000), None, "recovered");
        assert_eq!(read(612_000_000), Some(612), "below again");
        assert_eq!(slow.drop_mhz(None), None, "unreadable");
        assert_eq!(
            slow.drop_mhz(Some(cpu_at(612_000_000))),
            None,
            "an unreadable clock is no recovery"
        );
    }
}
