//! The clocks the title holds for each [`ClockStage`], and the way back to the system's own.
//!
//! A running overclock module takes the ask: it keeps the rates through sleep on its own, and
//! puts back whatever clkrst sets (with no profile, Horizon OC resets the clock in 100-300 ms).
//! Without one the title sets clkrst itself. The system puts its own rates back on a wake, on a
//! return from HOME and on a dock change (NOTES, 2026-11-07), so the title asks again at those
//! events, and once more [`AGAIN_AFTER`] later in case the system moved them after the event.
//! A module's hold outlives the title, so the first stage replaces whatever a crash left, and
//! [`release`] runs on every way out.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use pf_nx_app::platform::{ClockStage, OperationMode};
use pf_nx_sys::ocmod::{self, OcModule};
use pf_nx_sys::{clk, result_code};

/// After an event that moves the clocks, how long until the title asks once more.
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
/// The system's own rates in [`CLOCKS`]' order, for a title with no module; 0 is unknown.
static STOCK: [AtomicU32; 3] = [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];

fn module() -> Option<OcModule> {
    *MODULE.get_or_init(|| {
        // The first read comes before any ask, so it is what the system chose.
        if let Some(c) = clk::clocks() {
            store_stock(&[c.cpu_hz, c.gpu_hz, c.emc_hz]);
        }
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
    /// With no module: when to ask once more after an event that moves the clocks.
    again: Option<Instant>,
    /// The console changed mode, so the system's rates did too; read them before the next ask.
    restock: bool,
    keep_lines: u32,
}

impl ClockHold {
    pub fn new(mode: OperationMode) -> Self {
        Self {
            stage: None,
            mode,
            again: None,
            restock: false,
            keep_lines: 0,
        }
    }

    pub fn set_stage(&mut self, stage: ClockStage) {
        tracing::info!("clocks: {stage:?}");
        if stage == ClockStage::System {
            // Found first, so a module's hold from a crash is let go too; with no stage the
            // title stops asking again.
            module();
            release();
            self.stage = None;
            return;
        }
        self.stage = Some(stage);
        self.apply();
    }

    /// A dock change. With no module the system's rates change with it: they are read once the
    /// system has set them, at the performance-mode message or [`AGAIN_AFTER`] from here.
    pub fn mode_changed(&mut self, mode: OperationMode) {
        self.mode = mode;
        if module().is_some() {
            self.apply();
        } else if self.stage.is_some() {
            self.restock = true;
            self.again = Some(Instant::now() + AGAIN_AFTER);
        }
    }

    /// The system may have put its own clocks back: a wake, a return to focus, a new performance
    /// mode. With no module the title asks again now and once more [`AGAIN_AFTER`] later.
    pub fn system_moved(&mut self, why: &str) {
        if self.stage.is_none() || module().is_some() {
            return;
        }
        if self.keep_lines < KEEP_LINES {
            self.keep_lines += 1;
            tracing::info!("clocks: {why}, asking again");
        }
        self.reask();
        self.again = Some(Instant::now() + AGAIN_AFTER);
    }

    /// Once a frame: the second ask after an event, when it is due. Reads no clock otherwise.
    pub fn tick(&mut self) {
        if self.again.is_some_and(|at| Instant::now() >= at) {
            self.again = None;
            self.reask();
        }
    }

    /// Asks again, reading the system's rates first after a dock change.
    fn reask(&mut self) {
        if std::mem::take(&mut self.restock) {
            if let Some(c) = clk::clocks() {
                store_stock(&[c.cpu_hz, c.gpu_hz, c.emc_hz]);
            }
        }
        self.apply();
    }

    fn apply(&mut self) {
        let Some(stage) = self.stage else { return };
        let targets = stage.targets(self.mode);
        let wanted = [targets.cpu_hz, targets.gpu_hz, targets.emc_hz];
        if let Some(module) = module() {
            for ((id, _), hz) in CLOCKS.into_iter().zip(wanted) {
                if let Err(rc) = module.hold(id, hz.unwrap_or(0)) {
                    tracing::warn!("clocks: {} refused module {id}: {}", module.service(), result_code(rc));
                }
            }
            return;
        }
        for (((_, id), hz), stock) in CLOCKS.into_iter().zip(wanted).zip(&STOCK) {
            let hz = hz.unwrap_or_else(|| stock.load(Ordering::Relaxed));
            if hz == 0 {
                continue;
            }
            if let Err(rc) = clk::set_rate(id, hz) {
                tracing::warn!("clocks: clkrst refused {id:#x}: {}", result_code(rc));
            }
        }
    }
}
