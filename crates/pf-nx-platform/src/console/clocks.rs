//! The clocks the title holds for each [`ClockStage`], and the way back to the system's own.
//!
//! The title only raises a clock. Each one runs at the rate the system or the player's overclock
//! profile set, read before the title asks for anything; a stage holds a clock only where that
//! rate is below the stage's. A memory clock a player runs at 2400 stays there (issue 30's
//! console froze on a runtime memory change).
//!
//! A running overclock module takes the ask: it keeps the rates through sleep on its own, and
//! puts back whatever clkrst sets (with no profile, Horizon OC resets the clock in 100-300 ms).
//! Without one the title sets clkrst itself. The system puts its own rates back on a wake, on a
//! return from HOME and on a dock change (NOTES, 2026-11-07), so the title asks again at those
//! events, with a module too, and once more [`AGAIN_AFTER`] later in case the system moved them
//! after the event. Between events it reads nothing.
//!
//! A module's hold outlives the title, so the first stage lets go of whatever a crash left and
//! reads the rates once the module has put its own back. [`release`] runs on every way out.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use pf_nx_app::platform::{ClockStage, Clocks, OperationMode};
use pf_nx_sys::ocmod::{self, OcModule};
use pf_nx_sys::{clk, result_code};

/// After an event that moves the clocks, how long until the title asks once more. Also how long
/// a module takes to put its own rates back once a hold is let go (about 0.3 s, NOTES).
const AGAIN_AFTER: Duration = Duration::from_millis(500);
/// Log lines for clocks asked again, so a console that wakes all day cannot fill the log.
const KEEP_LINES: u32 = 20;

/// CPU, GPU and memory: the module's id, then clkrst's.
const CLOCKS: [(u32, u32); 3] = [
    (ocmod::MODULE_CPU, clk::MODULE_CPU),
    (ocmod::MODULE_GPU, clk::MODULE_GPU),
    (ocmod::MODULE_MEM, clk::MODULE_EMC),
];

/// The module found at the first stage. Statics, so a panic on any thread can reach them.
static MODULE: OnceLock<Option<OcModule>> = OnceLock::new();
/// The rates the system or the player's profile runs, in [`CLOCKS`]' order; 0 is unknown.
static STOCK: [AtomicU32; 3] = [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];

fn module() -> Option<OcModule> {
    *MODULE.get_or_init(|| {
        // The first read comes before any ask, so it is what the system chose. Under a module it
        // may be a crash's hold, so the first stage reads again once that is let go.
        read_stock();
        match OcModule::find() {
            Ok(Some(module)) => {
                tracing::info!("clocks: asking {}", module.service());
                Some(module)
            }
            Ok(None) => {
                tracing::info!("clocks: no overclock module, setting them directly");
                None
            }
            Err(rc) => {
                tracing::warn!("clocks: sm could not say ({}), setting them directly", result_code(rc));
                None
            }
        }
    })
}

fn read_stock() {
    if let Some(c) = clk::clocks() {
        store_stock(&[c.cpu_hz, c.gpu_hz, c.emc_hz]);
    }
}

fn store_stock(rates: &[u32; 3]) {
    for (slot, hz) in STOCK.iter().zip(rates) {
        slot.store(*hz, Ordering::Relaxed);
    }
}

/// Hands the clocks back. Safe on any thread and more than once; a title that never asked
/// touches nothing.
pub fn release() {
    match MODULE.get() {
        Some(Some(module)) => {
            for (id, _) in CLOCKS {
                let _ = module.hold(id, 0);
            }
        }
        Some(None) => {
            for ((_, id), stock) in CLOCKS.into_iter().zip(&STOCK) {
                let hz = stock.load(Ordering::Relaxed);
                if hz != 0 {
                    let _ = clk::set_rate(id, hz);
                }
            }
        }
        None => {}
    }
}

/// The stage in force, kept up on the main thread.
pub struct ClockHold {
    stage: Option<ClockStage>,
    mode: OperationMode,
    /// The first stage has run: a crash's hold is let go and the rates are being read.
    started: bool,
    /// When to ask once more after an event that moves the clocks.
    again: Option<Instant>,
    /// The rates the system or the profile runs changed (a dock change, the first stage under a
    /// module), so the next ask reads them first and nothing is asked before it.
    restock: bool,
    keep_lines: u32,
}

impl ClockHold {
    pub fn new(mode: OperationMode) -> Self {
        Self {
            stage: None,
            mode,
            started: false,
            again: None,
            restock: false,
            keep_lines: 0,
        }
    }

    pub fn set_stage(&mut self, stage: ClockStage) {
        tracing::info!("clocks: {stage:?}");
        if !std::mem::replace(&mut self.started, true) && module().is_some() {
            // A hold a crash left would read as the profile's rate: let go of it, then read.
            release();
            self.restock = true;
            self.again = Some(Instant::now() + AGAIN_AFTER);
        }
        if stage == ClockStage::System {
            // With no stage the title stops asking again.
            release();
            self.stage = None;
            return;
        }
        self.stage = Some(stage);
        if !self.restock {
            self.apply();
        }
    }

    /// A dock change: the rates the system or the profile runs change with it. A module's holds
    /// are let go so it can put the new ones back, and they are read [`AGAIN_AFTER`] from here.
    pub fn mode_changed(&mut self, mode: OperationMode) {
        self.mode = mode;
        if self.stage.is_none() {
            return;
        }
        if module().is_some() {
            release();
        }
        self.restock = true;
        self.again = Some(Instant::now() + AGAIN_AFTER);
    }

    /// The system may have put its own clocks back: a wake, a return to focus, a new performance
    /// mode. The title asks again now and once more [`AGAIN_AFTER`] later.
    pub fn system_moved(&mut self, why: &str) {
        if self.stage.is_none() || self.restock {
            return;
        }
        if self.keep_lines < KEEP_LINES {
            self.keep_lines += 1;
            tracing::info!("clocks: {why}, asking again");
        }
        self.apply();
        self.again = Some(Instant::now() + AGAIN_AFTER);
    }

    /// Once a frame: the second ask after an event, when it is due. Reads no clock otherwise.
    pub fn tick(&mut self) {
        if self.again.is_some_and(|at| Instant::now() >= at) {
            self.again = None;
            if std::mem::take(&mut self.restock) {
                read_stock();
            }
            self.apply();
        }
    }

    fn apply(&mut self) {
        let Some(stage) = self.stage else { return };
        let targets = stage.targets(self.mode);
        let stock = STOCK.each_ref().map(|s| s.load(Ordering::Relaxed));
        let running = Clocks {
            cpu_hz: stock[0],
            gpu_hz: stock[1],
            emc_hz: stock[2],
        };
        let held = targets.above(running);
        if self.keep_lines < KEEP_LINES {
            tracing::info!("clocks: {stage:?} {}", held.describe(running));
        }
        let wanted = [held.cpu_hz, held.gpu_hz, held.emc_hz];
        if let Some(module) = module() {
            for ((id, _), hz) in CLOCKS.into_iter().zip(wanted) {
                if let Err(rc) = module.hold(id, hz.unwrap_or(0)) {
                    tracing::warn!("clocks: {} refused module {id}: {}", module.service(), result_code(rc));
                }
            }
            return;
        }
        for (((_, id), hz), stock) in CLOCKS.into_iter().zip(wanted).zip(stock) {
            let hz = hz.unwrap_or(stock);
            if hz == 0 {
                continue;
            }
            if let Err(rc) = clk::set_rate(id, hz) {
                tracing::warn!("clocks: clkrst refused {id:#x}: {}", result_code(rc));
            }
        }
    }
}
