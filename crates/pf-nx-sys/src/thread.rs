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
