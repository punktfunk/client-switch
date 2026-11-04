//! Check 7: whether the title can set the CPU clock, and what the system does to it after.
//!
//! Y in the first seconds of a run starts it in place of the other checks. The tester asks for a
//! rate with a button, then sleeps, docks or leaves; every change of the clocks, focus or mode
//! becomes a line. The rate is never put back on the way out: the next run's first line shows
//! what the system left. Runs add to `probe-clock.log`, so one file holds a whole session.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pf_nx_sys::clk::{self, Clocks};
use pf_nx_sys::{applet, hid, result_code};

use crate::{nx, Out};

/// `HidNpadButton`'s bits.
const BUTTON_B: u64 = 1 << 1;
const BUTTON_X: u64 = 1 << 2;
const BUTTON_Y: u64 = 1 << 3;
const BUTTON_PLUS: u64 = 1 << 10;

/// The official top rate, a middle one for the menu, and the rate games run at.
const CPU_TOP_HZ: u32 = 1_785_000_000;
const CPU_MIDDLE_HZ: u32 = 1_224_000_000;
const CPU_STOCK_HZ: u32 = 1_020_000_000;

const ASK_WINDOW: Duration = Duration::from_secs(5);
const FRAME: Duration = Duration::from_micros(16_667);
/// Between reads of the clocks: under the rate a sysmodule could rewrite them at, so a rewrite
/// shows as its own line.
const SAMPLE: Duration = Duration::from_millis(100);
/// A line with nothing new, so a quiet stretch reads as held and not as a stopped probe.
const BEAT: Duration = Duration::from_secs(10);

/// Runs the check if Y comes within [`ASK_WINDOW`]; `true` once it ran.
pub fn run_if_asked(dir: &Path, out: &mut Out) -> bool {
    let mut pads = match hid::Pads::init(1) {
        Ok(pads) => pads,
        Err(e) => {
            out.line(&format!("probe 7 clock fail pads={e:#}"));
            return false;
        }
    };
    out.line("Y now: clock test. Wait 5 s: all the other checks.");
    let start = Instant::now();
    while start.elapsed() < ASK_WINDOW && applet::main_loop() {
        if buttons(&mut pads) & BUTTON_Y != 0 {
            run(dir, out, &mut pads);
            return true;
        }
        nx::screen_flush();
        std::thread::sleep(FRAME);
    }
    false
}

/// A line on the screen, in `probe.log` and in `probe-clock.log`.
struct Lines<'a> {
    out: &'a mut Out,
    file: Option<File>,
}

impl Lines<'_> {
    fn say(&mut self, text: &str) {
        let line = format!("probe 7 clock {} {text}", stamp());
        self.out.line(&line);
        if let Some(file) = self.file.as_mut() {
            let _ = writeln!(file, "{line}").and_then(|()| file.sync_data());
        }
    }
}

fn run(dir: &Path, out: &mut Out, pads: &mut hid::Pads) {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("probe-clock.log"))
        .ok();
    let mut lines = Lines { out, file };
    let mut seen = clk::clocks();
    lines.say(&format!(
        "start {:?} {:?} {:?} {}",
        applet::applet_type(),
        applet::operation_mode(),
        applet::focus_state(),
        show(seen)
    ));
    lines.say("X: CPU 1785. Y: CPU 1224. B: CPU 1020. Plus: leave without restoring.");

    let (tx, events) = mpsc::channel();
    let _hook = applet::hook(move |e| {
        let _ = tx.send(e);
    });
    let mut held = buttons(pads);
    let mut sampled = Instant::now();
    let mut beat = Instant::now();
    while applet::main_loop() {
        for event in events.try_iter() {
            lines.say(&format!(
                "event {event:?} {:?} {:?} {}",
                applet::operation_mode(),
                applet::focus_state(),
                show(clk::clocks())
            ));
        }

        let now = buttons(pads);
        let pressed = now & !held;
        held = now;
        if pressed & BUTTON_PLUS != 0 {
            lines.say(&format!("leave {}", show(clk::clocks())));
            return;
        }
        for (button, hz) in [
            (BUTTON_X, CPU_TOP_HZ),
            (BUTTON_Y, CPU_MIDDLE_HZ),
            (BUTTON_B, CPU_STOCK_HZ),
        ] {
            if pressed & button != 0 {
                let answer = match clk::set_rate(clk::MODULE_CPU, hz) {
                    Ok(()) => "ok".to_owned(),
                    Err(rc) => format!("FAIL({})", result_code(rc)),
                };
                lines.say(&format!("ask cpu={} {answer} {}", hz / 1_000_000, show(clk::clocks())));
            }
        }

        if sampled.elapsed() >= SAMPLE {
            sampled = Instant::now();
            let clocks = clk::clocks();
            if clocks != seen {
                lines.say(&format!("change {} -> {}", show(seen), show(clocks)));
                seen = clocks;
                beat = Instant::now();
            }
        }
        if beat.elapsed() >= BEAT {
            beat = Instant::now();
            lines.say(&format!("held {}", show(seen)));
        }
        nx::screen_flush();
        std::thread::sleep(FRAME);
    }
    lines.say("closed by the system");
}

/// The buttons down on player 1 or the handheld.
fn buttons(pads: &mut hid::Pads) -> u64 {
    pads.update();
    pads.sample(0).map_or(0, |pad| pad.buttons)
}

fn show(clocks: Option<Clocks>) -> String {
    clocks.map_or_else(
        || "clocks=unreadable".to_owned(),
        |c| {
            format!(
                "cpu={} gpu={} emc={}",
                c.cpu_hz / 1_000_000,
                c.gpu_hz / 1_000_000,
                c.emc_hz / 1_000_000
            )
        },
    )
}

/// The wall clock, UTC: unlike a monotonic count, it still runs through sleep.
fn stamp() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = now.as_secs() % 86_400;
    format!(
        "{:02}:{:02}:{:02}.{:03}Z",
        secs / 3600,
        secs / 60 % 60,
        secs % 60,
        now.subsec_millis()
    )
}
