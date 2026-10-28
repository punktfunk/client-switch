//! The C symbols the target's link wraps (`--wrap=` in `toolchain/aarch64-nintendo-horizon.json`)
//! or newlib expects and libnx leaves out. After bevy-horizon's `compat.rs` (MIT OR Apache-2.0).
//!
//! Every binary calls [`link_guard`] first: libnx references `userAppInit` weakly, and a weak
//! reference never pulls an archive member into the link.

use core::arch::asm;
use core::ffi::{c_char, c_int, c_long, c_void};

extern "C" {
    fn __real_threadCreate(
        t: *mut c_void,
        entry: ThreadFunc,
        arg: *mut c_void,
        stack_mem: *mut c_void,
        stack_sz: usize,
        prio: c_int,
        cpuid: c_int,
    ) -> u32;
    fn __real_fchmod(fd: c_int, mode: u32) -> c_int;
    fn __real_rename(old: *const c_char, new: *const c_char) -> c_int;
    fn __real_rmdir(path: *const c_char) -> c_int;
    fn __errno() -> *mut c_int;
}

/// libnx `ThreadFunc`.
type ThreadFunc = unsafe extern "C" fn(arg: *mut c_void);

/// newlib's errno values.
const ENOSYS: c_int = 88;
const EFAULT: c_int = 14;

/// Keeps this module in the link. A binary's own object references these strongly, so the
/// linker extracts the member that defines them.
#[inline(never)]
pub fn link_guard() {
    let keep: [*const (); 10] = [
        userAppInit as *const (),
        __wrap_threadCreate as *const (),
        __wrap_fchmod as *const (),
        __wrap_rename as *const (),
        __wrap_rmdir as *const (),
        _init as *const (),
        _fini as *const (),
        sysconf as *const (),
        getrandom as *const (),
        _getentropy_r as *const (),
    ];
    std::hint::black_box(keep);
}

/// Copies libnx's `tls_tp` (0x1f8 into the thread's TLS page) into `TPIDR_EL0`, where Rust's
/// local-exec thread-locals look. The function that calls this must not touch a thread-local.
#[inline(never)]
fn set_thread_pointer() {
    // SAFETY: TPIDRRO_EL0 points at this thread's TLS page, whose `tls_tp` libnx fills before
    // any code of ours runs on the thread; TPIDR_EL0 is this thread's own register.
    unsafe {
        asm!(
            "mrs {tp}, tpidrro_el0",
            "ldr {tp}, [{tp}, #0x1f8]",
            "msr tpidr_el0, {tp}",
            tp = out(reg) _,
            options(nostack, preserves_flags),
        );
    }
}

/// libnx's hook once its services are up, before std's runtime touches the first thread-local.
#[no_mangle]
pub extern "C" fn userAppInit() {
    set_thread_pointer();
}

/// Every thread, std's included, is created here: the new thread starts in `thread_start`,
/// which sets its thread pointer before the real entry runs.
///
/// # Safety
/// As libnx's `threadCreate`.
#[no_mangle]
pub unsafe extern "C" fn __wrap_threadCreate(
    t: *mut c_void,
    entry: ThreadFunc,
    arg: *mut c_void,
    stack_mem: *mut c_void,
    stack_sz: usize,
    prio: c_int,
    cpuid: c_int,
) -> u32 {
    let start = Box::into_raw(Box::new((entry, arg)));
    // SAFETY: the caller's arguments, with `thread_start` owning `start` once the thread runs.
    let rc = unsafe { __real_threadCreate(t, thread_start, start.cast(), stack_mem, stack_sz, prio, cpuid) };
    if rc != 0 {
        // SAFETY: no thread was created, so `start` is still only ours.
        drop(unsafe { Box::from_raw(start) });
    }
    rc
}

unsafe extern "C" fn thread_start(start: *mut c_void) {
    set_thread_pointer();
    // SAFETY: `start` is the box `__wrap_threadCreate` leaked for this thread alone.
    let (entry, arg) = *unsafe { Box::from_raw(start.cast::<(ThreadFunc, *mut c_void)>()) };
    // SAFETY: the entry and argument the thread's creator passed to `threadCreate`.
    unsafe { entry(arg) };
}

/// Horizon's file systems have no permission bits and fsdev fails `fchmod` with ENOSYS, which
/// would fail std's `fs::copy`; there is nothing to set.
///
/// # Safety
/// As `fchmod`.
#[no_mangle]
pub unsafe extern "C" fn __wrap_fchmod(fd: c_int, mode: u32) -> c_int {
    // SAFETY: the caller's arguments; errno is this thread's.
    unsafe {
        let rc = __real_fchmod(fd, mode);
        if rc == -1 && *__errno() == ENOSYS {
            return 0;
        }
        rc
    }
}

// ponytail: rename and rmdir pass through; replace-on-rename and the ENOTEMPTY mapping come
// when the app renames or removes folders.
/// # Safety
/// As `rename`.
#[no_mangle]
pub unsafe extern "C" fn __wrap_rename(old: *const c_char, new: *const c_char) -> c_int {
    // SAFETY: the caller's arguments.
    unsafe { __real_rename(old, new) }
}

/// # Safety
/// As `rmdir`.
#[no_mangle]
pub unsafe extern "C" fn __wrap_rmdir(path: *const c_char) -> c_int {
    // SAFETY: the caller's argument.
    unsafe { __real_rmdir(path) }
}

/// libnx's crt0 runs `.init_array` through these; rust-lld links no crti/crtn to define them.
#[no_mangle]
pub extern "C" fn _init() {}

#[no_mangle]
pub extern "C" fn _fini() {}

/// Applications run on cores 0-2; core 3 is the system's.
#[no_mangle]
pub extern "C" fn sysconf(name: c_int) -> c_long {
    match name {
        8 => 0x1000, // _SC_PAGESIZE
        9 | 10 => 3, // _SC_NPROCESSORS_CONF, _SC_NPROCESSORS_ONLN
        _ => -1,
    }
}

/// std's hashmap keys and the getrandom legs read this.
///
/// # Safety
/// `buf` is writable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn getrandom(buf: *mut c_void, len: usize, _flags: u32) -> isize {
    if buf.is_null() && len != 0 {
        // SAFETY: errno is this thread's.
        unsafe { *__errno() = EFAULT };
        return -1;
    }
    // SAFETY: the caller's buffer, writable for `len` bytes.
    crate::random::fill(unsafe { core::slice::from_raw_parts_mut(buf.cast::<u8>(), len) });
    len as isize
}

/// newlib's `getentropy` and `arc4random` call this, and nothing in devkitA64 defines it.
///
/// # Safety
/// `buf` is writable for `len` bytes; `reent` is newlib's, whose first field is errno.
#[no_mangle]
pub unsafe extern "C" fn _getentropy_r(reent: *mut c_int, buf: *mut c_void, len: usize) -> c_int {
    if buf.is_null() && len != 0 {
        if !reent.is_null() {
            // SAFETY: `struct _reent` starts with `int _errno`.
            unsafe { *reent = EFAULT };
        }
        return -1;
    }
    // SAFETY: the caller's buffer, writable for `len` bytes.
    crate::random::fill(unsafe { core::slice::from_raw_parts_mut(buf.cast::<u8>(), len) });
    0
}
