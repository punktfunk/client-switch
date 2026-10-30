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

/// libnx `MemoryInfo` (kernel/svc.h), what `svcQueryMemory` fills.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MemoryInfo {
    pub addr: u64,
    pub size: u64,
    /// `MemoryType`, 5 for heap.
    pub kind: u32,
    /// `MemoryAttribute` bits.
    pub attr: u32,
    /// `Permission` bits.
    pub perm: u32,
    pub ipc_refcount: u32,
    pub device_refcount: u32,
    pub padding: u32,
}

/// libnx `Perm_Rw`.
const PERM_RW: u32 = 3;
/// `MemAttr_IsBorrowed`: locked, as transfer memory and mapped thread stacks leave the pages.
pub const ATTR_LOCKED: u32 = 1 << 0;
/// The attributes the kernel ignores in a non-secure IPC buffer, such as the one hbl reads the
/// next NRO into: `IpcMapped`, `DeviceMapped`, `PermissionLocked` (Atmosphère
/// `KPageTableBase::SetupForIpcClient`). Any other, `Borrowed` in particular, fails with 2001-0106.
const ATTRS_IPC_IGNORES: u32 = (1 << 1) | (1 << 2) | (1 << 4);
/// `MemAttr_IsDeviceMapped` and `MemAttr_IsUncached`: heap pages an nvmap object still holds.
pub const ATTRS_NVMAP: u32 = (1 << 2) | (1 << 3);

/// A run of heap pages hbl's read of the next NRO would fail on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeldBlock {
    pub start: u64,
    pub end: u64,
    pub kind: u32,
    pub perm: u32,
    pub attr: u32,
}

/// Whether hbl can read into a block with these permission and attribute bits.
pub fn loader_can_read(perm: u32, attr: u32) -> bool {
    perm & PERM_RW == PERM_RW && attr & !ATTRS_IPC_IGNORES == 0
}

#[cfg(target_os = "horizon")]
mod heap_ffi {
    use super::MemoryInfo;
    use core::ffi::c_void;

    extern "C" {
        pub fn svcQueryMemory(info: *mut MemoryInfo, page_info: *mut u32, addr: u64) -> u32;
        pub fn envHasHeapOverride() -> bool;
        pub fn envGetHeapOverrideAddr() -> *mut c_void;
        pub fn envGetHeapOverrideSize() -> u64;
        /// newlib's sbrk bounds, which libnx's `__libnx_initheap` sets. NVK lowers the end to take
        /// GPU memory from the top of the heap.
        pub static fake_heap_start: *mut u8;
        pub static fake_heap_end: *mut u8;
        /// applet.h: 1 makes `appletExit` ask AM to end the process instead of returning to hbl.
        pub static mut __nx_applet_exit_mode: u32;
    }
}

/// The malloc heap's bounds: the loader's heap override, else the run of heap-type blocks up
/// from `fake_heap_start`. NVK's region above `fake_heap_end` is inside.
#[cfg(target_os = "horizon")]
fn heap_bounds() -> Result<(u64, u64), (u64, u32)> {
    // SAFETY: plain reads of libnx's and newlib's globals, set before `main`.
    let (start, limit, over) = unsafe {
        let start = core::ptr::read_volatile(&raw const heap_ffi::fake_heap_start) as u64;
        let limit = core::ptr::read_volatile(&raw const heap_ffi::fake_heap_end) as u64;
        let over = heap_ffi::envHasHeapOverride()
            .then(|| (heap_ffi::envGetHeapOverrideAddr() as u64).saturating_add(heap_ffi::envGetHeapOverrideSize()));
        (start, limit, over)
    };
    let end = match over {
        Some(end) => end,
        None => {
            let mut addr = start;
            loop {
                let info = query(addr)?;
                let block_end = info.addr.saturating_add(info.size);
                if block_end <= addr || info.kind != 5 {
                    break addr;
                }
                addr = block_end;
            }
        }
    };
    Ok((start, end.max(limit)))
}

#[cfg(target_os = "horizon")]
fn query(addr: u64) -> Result<MemoryInfo, (u64, u32)> {
    let (mut info, mut page) = (MemoryInfo::default(), 0u32);
    // SAFETY: both out-pointers are locals.
    let rc = unsafe { heap_ffi::svcQueryMemory(&mut info, &mut page, addr) };
    if rc != 0 {
        return Err((addr, rc));
    }
    Ok(info)
}

/// Every heap block hbl could not read the next NRO into. Fails with the address and result of
/// a query the kernel refused.
#[cfg(target_os = "horizon")]
pub fn held_heap() -> Result<Vec<HeldBlock>, (u64, u32)> {
    let (start, end) = heap_bounds()?;
    let mut held = Vec::new();
    let mut addr = start;
    while addr < end {
        let info = query(addr)?;
        let block_end = info.addr.saturating_add(info.size).min(end);
        if block_end <= addr {
            return Err((addr, 0));
        }
        if !loader_can_read(info.perm, info.attr) {
            held.push(HeldBlock {
                start: addr,
                end: block_end,
                kind: info.kind,
                perm: info.perm,
                attr: info.attr,
            });
        }
        addr = block_end;
    }
    Ok(held)
}

/// Makes the exit end the process through AM, to HOME, instead of returning to hbl.
#[cfg(target_os = "horizon")]
pub fn exit_through_am() {
    // SAFETY: libnx reads the flag once, in `appletExit`, on this same thread.
    unsafe { core::ptr::write_volatile(&raw mut heap_ffi::__nx_applet_exit_mode, 1) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_loader_reads_through_what_ipc_ignores_only() {
        assert!(loader_can_read(PERM_RW, 0));
        assert!(loader_can_read(PERM_RW, 1 << 2));
        // A thread stack or transfer memory: locked, no access.
        assert!(!loader_can_read(0, ATTR_LOCKED));
        // An uncached nvmap object.
        assert!(!loader_can_read(PERM_RW, 1 << 3));
        assert!(!loader_can_read(1, 0));
    }
}
