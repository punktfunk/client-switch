//! The process's memory, from the kernel. Extern from `kernel/svc.h`, libnx 4.12.0.
//!
//! libnx's `__libnx_initheap` (or the loader's heap override) hands malloc nearly all of
//! `total` at startup, so `used` already counts the whole heap. Free heap is malloc's figure,
//! newlib's `mallinfo`; libnx exposes none, and this crate does not bind newlib.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemInfo {
    pub total: u64,
    pub used: u64,
}

/// `CUR_PROCESS_HANDLE`.
pub const CUR_PROCESS_HANDLE: u32 = 0xFFFF_8001;
/// `InfoType_TotalMemorySize`, `InfoType_UsedMemorySize`.
pub const INFO_TYPE_TOTAL_MEMORY_SIZE: u32 = 6;
pub const INFO_TYPE_USED_MEMORY_SIZE: u32 = 7;

#[cfg(target_os = "horizon")]
extern "C" {
    fn svcGetInfo(out: *mut u64, id0: u32, handle: u32, id1: u64) -> u32;
}

/// Total and used bytes of the process; a figure the kernel refuses reads 0.
#[cfg(target_os = "horizon")]
pub fn info() -> MemInfo {
    let get = |id| {
        let mut v = 0u64;
        // SAFETY: `v` is a valid out-pointer; the pseudo-handle names this process.
        let rc = unsafe { svcGetInfo(&mut v, id, CUR_PROCESS_HANDLE, 0) };
        if rc == 0 {
            v
        } else {
            0
        }
    };
    MemInfo {
        total: get(INFO_TYPE_TOTAL_MEMORY_SIZE),
        used: get(INFO_TYPE_USED_MEMORY_SIZE),
    }
}
