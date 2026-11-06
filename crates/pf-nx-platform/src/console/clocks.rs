//! The clocks the title holds for each [`ClockStage`], and the way back to the system's own.
//!
//! A running overclock module takes the ask: it keeps the rates through sleep on its own, and
//! puts back whatever clkrst sets (with no profile, Horizon OC resets the clock in 100-300 ms).
//! Without one the title sets clkrst itself and asks again when the system moves the clocks.
//! A module's hold outlives the title, so the first stage replaces whatever a crash left, and
//! [`release`] runs on every way out.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use pf_nx_app::platform::{ClockStage, OperationMode};
use pf_nx_sys::ocmod::{self, OcModule};
use pf_nx_sys::{clk, result_code};

/// Between looks at the clocks when no module holds them.
const KEEP_EVERY: Duration = Duration::from_secs(1);
/// Log lines for clocks asked again, so a clock the system never leaves cannot fill the log.
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
    /// What the clocks read after the last ask, with no module.
    applied: Option<clk::Clocks>,
    looked: Instant,
    /// The console changed mode, so the system's rates did too; read them at the next look.
    restock: bool,
    keep_lines: u32,
}

impl ClockHold {
    pub fn new(mode: OperationMode) -> Self {
        Self {
            stage: None,
            mode,
            applied: None,
            looked: Instant::now(),
            restock: false,
            keep_lines: 0,
        }
    }

    pub fn set_stage(&mut self, stage: ClockStage) {
        tracing::info!("clocks: {stage:?}");
        self.stage = Some(stage);
        self.apply();
    }

    pub fn mode_changed(&mut self, mode: OperationMode) {
        self.mode = mode;
        if module().is_some() {
            self.apply();
        } else {
            self.restock = true;
        }
    }

    /// Once a frame: with no module, sleep and dock changes put the clocks back under the title.
    pub fn keep(&mut self) {
        if self.stage.is_none() || self.looked.elapsed() < KEEP_EVERY || module().is_some() {
            return;
        }
        self.looked = Instant::now();
        let now = clk::clocks();
        if std::mem::take(&mut self.restock) {
            if let Some(c) = now {
                store_stock(&[c.cpu_hz, c.gpu_hz, c.emc_hz]);
            }
        } else if now == self.applied {
            return;
        } else if self.keep_lines < KEEP_LINES {
            self.keep_lines += 1;
            tracing::info!("clocks: moved under the title, asking again");
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
        self.applied = clk::clocks();
        self.looked = Instant::now();
    }
}
