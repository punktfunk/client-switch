//! The current thread's priority and cores. Externs from `kernel/svc.h` and `kernel/thread.h`,
//! libnx 4.12.0.

/// Horizon priorities run 0x00 (most urgent) to 0x3F. 0x2C is the main thread's usual priority;
/// 0x3B on cores 0–2 (0x3F on core 3) enables preemption among equals (thread.h).
pub const PRIORITY_RANGE: core::ops::RangeInclusive<i32> = 0x00..=0x3F;

#[cfg(target_os = "horizon")]
mod ffi {
    extern "C" {
        pub fn threadGetCurHandle() -> u32;
        pub fn svcSetThreadPriority(handle: u32, priority: u32) -> u32;
        pub fn svcGetThreadCoreMask(preferred_core: *mut i32, affinity_mask: *mut u64, handle: u32) -> u32;
        pub fn svcGetThreadPriority(priority: *mut i32, handle: u32) -> u32;
        pub fn svcGetCurrentProcessorNumber() -> u32;
        pub fn svcGetInfo(out: *mut u64, id0: u32, handle: u32, id1: u64) -> u32;
    }
}

/// Sets the calling thread's priority, `0x00..=0x3F`, lower is more urgent. The process's
/// priority mask can refuse a value inside the range.
#[cfg(target_os = "horizon")]
pub fn set_current_priority(prio: i32) -> anyhow::Result<()> {
    anyhow::ensure!(
        PRIORITY_RANGE.contains(&prio),
        "set thread priority {prio:#x}: outside 0x00..=0x3f"
    );
    // SAFETY: the calling thread's own handle and a priority in range.
    let rc = unsafe { ffi::svcSetThreadPriority(ffi::threadGetCurHandle(), prio as u32) };
    if rc != 0 {
        return Err(crate::nx_error("set thread priority", rc));
    }
    Ok(())
}

/// The cores the calling thread may run on, one bit per core; 0 if the kernel refuses.
#[cfg(target_os = "horizon")]
pub fn core_mask() -> u64 {
    let (mut preferred, mut mask) = (0i32, 0u64);
    // SAFETY: both out-pointers are locals; the calling thread's own handle.
    let rc = unsafe { ffi::svcGetThreadCoreMask(&mut preferred, &mut mask, ffi::threadGetCurHandle()) };
    if rc == 0 {
        mask
    } else {
        0
    }
}

// The thread registry: every thread `rt`'s `threadCreate` wrapper starts, and main, with the
// core each first ran on, so a log can say where the title's threads run and how busy each core
// is. A thread only records where it woke up; its spawner names it.

/// Cores 0-2 run applications; core 3 is the system's, counted to show whether it stays unused.
pub const CORES: usize = 4;

/// One live thread as the registry last saw it.
#[derive(Clone, Debug, Default)]
pub struct ThreadSample {
    /// Creation order since launch.
    pub seq: u32,
    /// The name its spawner gave, once one has told the registry.
    pub name: Option<String>,
    /// The core it first ran on; `None` before it has.
    pub start_core: Option<u32>,
    /// Its preferred core and the cores it may run on, `None` where the kernel refused.
    pub preferred: Option<i32>,
    pub mask: Option<u64>,
    pub priority: Option<i32>,
    /// The share of the window it ran on each core; `None` on the first report, or where the
    /// kernel refused the count.
    pub busy: [Option<f32>; CORES],
}

/// What `report` found.
#[derive(Clone, Debug, Default)]
pub struct ThreadReport {
    /// The cores the process may run threads on.
    pub process_mask: Option<u64>,
    pub threads: Vec<ThreadSample>,
    /// Threads that started and ended since the last report, never seen live in one.
    pub gone: Vec<ThreadSample>,
    /// The first refused tick count's `Result`.
    pub tick_error: Option<u32>,
}

#[cfg(target_os = "horizon")]
mod registry {
    use std::sync::{Mutex, MutexGuard, PoisonError};
    use std::time::Instant;

    use super::{ffi, ThreadReport, ThreadSample, CORES};

    /// `svcGetInfo`'s ticks run at 19.2 MHz.
    const TICK_HZ: f64 = 19_200_000.0;
    /// `InfoType_CoreMask` and `InfoType_ThreadTickCount` (svc.h).
    const INFO_CORE_MASK: u32 = 0;
    const INFO_THREAD_TICK_COUNT: u32 = 0xF000_0002;
    /// `CUR_PROCESS_HANDLE`.
    const CUR_PROCESS: u32 = 0xFFFF_8001;
    const GONE_MAX: usize = 16;

    struct Entry {
        handle: u32,
        sample: ThreadSample,
        /// CPU ticks per core at the last report.
        ticks: [Option<u64>; CORES],
        reported: bool,
    }

    struct Registry {
        live: Vec<Entry>,
        gone: Vec<ThreadSample>,
        next_seq: u32,
        last_report: Option<Instant>,
    }

    impl Registry {
        fn entry(&mut self, handle: u32) -> &mut Entry {
            if let Some(i) = self.live.iter().position(|e| e.handle == handle) {
                return &mut self.live[i];
            }
            let seq = self.next_seq;
            self.next_seq += 1;
            self.live.push(Entry {
                handle,
                sample: ThreadSample {
                    seq,
                    ..ThreadSample::default()
                },
                ticks: [None; CORES],
                reported: false,
            });
            self.live.last_mut().expect("just pushed")
        }
    }

    static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
        live: Vec::new(),
        gone: Vec::new(),
        next_seq: 0,
        last_report: None,
    });

    fn registry() -> MutexGuard<'static, Registry> {
        REGISTRY.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The calling thread's handle and the core it runs on now.
    fn here() -> (u32, u32) {
        // SAFETY: no arguments; the calling thread's own handle and core.
        unsafe { (ffi::threadGetCurHandle(), ffi::svcGetCurrentProcessorNumber()) }
    }

    /// A thread was created, suspended, with `handle`: it takes the next number.
    pub(crate) fn created(handle: u32) {
        registry().entry(handle);
    }

    /// The calling thread is running: it records the core it woke up on.
    pub(crate) fn started() {
        let (handle, core) = here();
        registry().entry(handle).sample.start_core = Some(core);
    }

    /// The calling thread's entry returned: it leaves the registry before its handle closes.
    pub(crate) fn finished() {
        let (handle, _) = here();
        let mut registry = registry();
        if let Some(i) = registry.live.iter().position(|e| e.handle == handle) {
            let mut entry = registry.live.swap_remove(i);
            // Short-lived threads between two reports, as many as a line can carry.
            if !entry.reported && registry.gone.len() < GONE_MAX {
                (entry.sample.preferred, entry.sample.mask) = core_mask_of(handle);
                registry.gone.push(entry.sample);
            }
        }
    }

    /// Names the thread behind `pthread`, as its spawner knows it.
    ///
    /// # Safety
    /// `pthread` is a pthread newlib created (std's `JoinHandleExt::as_pthread_t`), not yet
    /// joined.
    pub unsafe fn name_spawned(pthread: usize, name: &str) {
        // newlib's `THRD_MAIN_HANDLE` is no pthread.
        if pthread == 0 || pthread == usize::MAX {
            return;
        }
        // SAFETY: the caller's live pthread, whose first field is libnx's `Thread`, whose first
        // is the handle.
        let handle = unsafe { (pthread as *const u32).read() };
        registry().entry(handle).sample.name = Some(name.to_string());
    }

    fn core_mask_of(handle: u32) -> (Option<i32>, Option<u64>) {
        let (mut preferred, mut mask) = (0i32, 0u64);
        // SAFETY: both out-pointers are locals; a live thread's handle.
        let rc = unsafe { ffi::svcGetThreadCoreMask(&mut preferred, &mut mask, handle) };
        if rc == 0 {
            (Some(preferred), Some(mask))
        } else {
            (None, None)
        }
    }

    /// The process's allowed cores (`InfoType_CoreMask`).
    pub fn process_core_mask() -> Option<u64> {
        let mut mask = 0u64;
        // SAFETY: a local out-pointer; the process's own pseudo-handle.
        let rc = unsafe { ffi::svcGetInfo(&mut mask, INFO_CORE_MASK, CUR_PROCESS, 0) };
        (rc == 0).then_some(mask)
    }

    /// Every live thread's cores, priority and share of each core since the last call, and the
    /// threads that came and went in between. The first call opens the window and names its
    /// caller `main` (the run loop's thread, which `rt` never starts), whose start core is
    /// where it runs then.
    pub fn report() -> ThreadReport {
        let now = Instant::now();
        let (caller, core) = here();
        let mut registry = registry();
        let window = match registry.last_report.replace(now) {
            Some(last) => Some(now.duration_since(last).as_secs_f64() * TICK_HZ),
            None => {
                let main = registry.entry(caller);
                main.sample.name.get_or_insert_with(|| "main".to_string());
                main.sample.start_core.get_or_insert(core);
                None
            }
        };
        let mut report = ThreadReport {
            process_mask: process_core_mask(),
            gone: std::mem::take(&mut registry.gone),
            ..ThreadReport::default()
        };
        for entry in &mut registry.live {
            entry.reported = true;
            let handle = entry.handle;
            (entry.sample.preferred, entry.sample.mask) = core_mask_of(handle);
            let mut priority = 0i32;
            // SAFETY: a local out-pointer; a live thread's handle.
            let rc = unsafe { ffi::svcGetThreadPriority(&mut priority, handle) };
            entry.sample.priority = (rc == 0).then_some(priority);
            for core in 0..CORES {
                let mut ticks = 0u64;
                // SAFETY: a local out-pointer; a live thread's handle and a core id.
                let rc = unsafe { ffi::svcGetInfo(&mut ticks, INFO_THREAD_TICK_COUNT, handle, core as u64) };
                if rc != 0 {
                    report.tick_error.get_or_insert(rc);
                }
                let ticks = (rc == 0).then_some(ticks);
                entry.sample.busy[core] = match (window, entry.ticks[core], ticks) {
                    (Some(window), Some(before), Some(after)) if window > 0.0 => {
                        Some((after.saturating_sub(before) as f64 / window) as f32)
                    }
                    _ => None,
                };
                entry.ticks[core] = ticks;
            }
            report.threads.push(entry.sample.clone());
        }
        report.threads.sort_by_key(|t| t.seq);
        report
    }
}

#[cfg(target_os = "horizon")]
pub(crate) use registry::{created, finished, started};
#[cfg(target_os = "horizon")]
pub use registry::{name_spawned, process_core_mask, report};
