//! The C symbols the target's link wraps (`--wrap=` in `toolchain/aarch64-nintendo-horizon.json`)
//! or newlib expects and libnx leaves out. After bevy-horizon's `compat.rs` (MIT OR Apache-2.0).
//!
//! Every binary calls [`link_guard`] first: libnx references `userAppInit` weakly, and a weak
//! reference never pulls an archive member into the link.

use core::arch::asm;
use core::ffi::{c_char, c_int, c_long, c_void};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

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
    fn __real_setsockopt(fd: c_int, level: c_int, name: c_int, value: *const c_void, len: u32) -> c_int;
    fn __errno() -> *mut c_int;
    fn threadClose(t: *mut c_void) -> u32;
    fn __libnx_free(p: *mut c_void);
    fn svcWaitSynchronization(index: *mut i32, handles: *const u32, count: i32, timeout_ns: u64) -> u32;
    fn svcSleepThread(nano: i64);
    fn memalign(align: usize, size: usize) -> *mut c_void;
}

/// libnx `ThreadFunc`.
type ThreadFunc = unsafe extern "C" fn(arg: *mut c_void);

/// newlib's errno values.
const ENOSYS: c_int = 88;
const EFAULT: c_int = 14;
const EEXIST: c_int = 17;
const ENOBUFS: c_int = 105;
const EINVAL: c_int = 22;
const ENOMEM: c_int = 12;

/// newlib's `SOL_SOCKET` and `SO_RCVBUF`.
const SOL_SOCKET: c_int = 0xffff;
const SO_RCVBUF: c_int = 0x1002;

/// Keeps this module in the link. A binary's own object references these strongly, so the
/// linker extracts the member that defines them.
#[inline(never)]
pub fn link_guard() {
    let keep: [*const (); 17] = [
        userAppInit as *const (),
        __wrap_threadCreate as *const (),
        __syscall_thread_detach as *const (),
        __wrap_fchmod as *const (),
        __wrap_rename as *const (),
        __wrap_rmdir as *const (),
        __wrap_setsockopt as *const (),
        _init as *const (),
        _fini as *const (),
        sysconf as *const (),
        getrandom as *const (),
        _getentropy_r as *const (),
        posix_memalign as *const (),
        dlopen as *const (),
        dlsym as *const (),
        dlclose as *const (),
        dlerror as *const (),
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
    // Frees the stacks of detached threads that have exited before taking another one.
    close_detached_threads(Duration::ZERO);
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

// std detaches a thread whenever its `JoinHandle` drops unjoined (tokio's blocking pool, the
// log writer, fire-and-forget spawns): `pthread_detach`. devkitPro newlib's calls the weak
// `__syscall_thread_detach`, which libnx 4.12 leaves undefined, so a detached thread that
// finished kept its stack mapped and the heap pages under it locked, and hbl faulted reading
// the next NRO over them at exit (2001-0106). A thread cannot unmap the stack it runs on, so
// detaching records it here, and it is closed as `pthread_join` would once its handle signals:
// at the next thread created or detached, and at exit. After bevy-horizon's `compat.rs`.

/// newlib.c's `THRD_MAIN_HANDLE`, what `pthread_self` returns on the main thread.
const MAIN_THREAD: usize = !0;
const DETACHED_SLOTS: usize = 256;
/// libnx `struct __pthread_t` pointers (`{ Thread thr; void *rc; }`, from `__libnx_alloc`); 0
/// is a free slot. Only the closer, holding `CLOSING`, empties slots.
static DETACHED: [AtomicUsize; DETACHED_SLOTS] = [const { AtomicUsize::new(0) }; DETACHED_SLOTS];
static DETACHED_COUNT: AtomicUsize = AtomicUsize::new(0);
static CLOSING: AtomicBool = AtomicBool::new(false);
/// Detached while every slot was taken: never closed.
static UNTRACKED: AtomicU32 = AtomicU32::new(0);
/// Exited detached threads `threadClose` refused, since launch.
static CLOSE_FAILED: AtomicU32 = AtomicU32::new(0);
/// `svcWaitSynchronization` takes at most 0x40 handles.
const MAX_WAIT_HANDLES: usize = 0x40;

/// libnx's missing `pthread_detach` back end. It never fails: std `debug_assert`s that.
///
/// # Safety
/// `thread` is a pthread newlib created, detached once.
#[no_mangle]
pub unsafe extern "C" fn __syscall_thread_detach(thread: *mut c_void) -> c_int {
    let thread = thread as usize;
    if thread == 0 || thread == MAIN_THREAD {
        return 0;
    }
    let tracked = DETACHED.iter().any(|slot| {
        slot.compare_exchange(0, thread, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    });
    if tracked {
        DETACHED_COUNT.fetch_add(1, Ordering::AcqRel);
    } else {
        UNTRACKED.fetch_add(1, Ordering::Relaxed);
    }
    close_detached_threads(Duration::ZERO);
    0
}

/// What [`close_detached_threads`] did.
#[derive(Clone, Copy, Debug, Default)]
pub struct DetachedThreads {
    /// Closed by this call.
    pub closed: u32,
    /// Detached and still running when it returned.
    pub running: u32,
    /// Since launch: exited ones `threadClose` refused, and ones detached past the slots.
    pub close_failed: u32,
    pub untracked: u32,
}

/// Closes every detached thread that has exited: unmaps its stack, which unlocks those heap
/// pages, closes its handle and frees it. A non-zero `wait` keeps waiting that long in all for
/// the running ones. `None` when another thread held the closer for all of `wait`.
pub fn close_detached_threads(wait: Duration) -> Option<DetachedThreads> {
    let deadline = (!wait.is_zero()).then(|| Instant::now() + wait);
    let mut result = DetachedThreads::default();
    if DETACHED_COUNT.load(Ordering::Acquire) != 0 {
        while CLOSING
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            match deadline {
                // SAFETY: sleeps the calling thread.
                Some(deadline) if Instant::now() < deadline => unsafe { svcSleepThread(1_000_000) },
                _ => return None,
            }
        }
        loop {
            let mut handles = [0u32; MAX_WAIT_HANDLES];
            let mut count = 0;
            result.running = 0;
            for slot in &DETACHED {
                let thread = slot.load(Ordering::Acquire);
                if thread == 0 {
                    continue;
                }
                // SAFETY: a live pthread from the slot, whose first field is libnx's `Thread`,
                // whose first is the handle; a thread's handle signals once it has exited.
                let handle = unsafe { (thread as *const u32).read() };
                let mut index = 0;
                // SAFETY: one handle, a zero timeout.
                if unsafe { svcWaitSynchronization(&mut index, &handle, 1, 0) } != 0 {
                    result.running += 1;
                    if count < MAX_WAIT_HANDLES {
                        handles[count] = handle;
                        count += 1;
                    }
                    continue;
                }
                slot.store(0, Ordering::Release);
                DETACHED_COUNT.fetch_sub(1, Ordering::AcqRel);
                // SAFETY: the thread has exited and nothing else holds it: detached, so never
                // joined. `threadClose` unmaps its stack; the pthread is libnx's allocation.
                if unsafe { threadClose(thread as *mut c_void) } == 0 {
                    // SAFETY: closed, so the pthread is ours alone to free, as `pthread_join` does.
                    unsafe { __libnx_free(thread as *mut c_void) };
                    result.closed += 1;
                } else {
                    CLOSE_FAILED.fetch_add(1, Ordering::Relaxed);
                }
            }
            let left = deadline.map_or(Duration::ZERO, |d| d.saturating_duration_since(Instant::now()));
            if count == 0 || left.is_zero() {
                break;
            }
            // Sleeps until one of them exits, the time is up, or a handle is bad.
            let mut index = 0;
            let nanos = u64::try_from(left.as_nanos()).unwrap_or(u64::MAX);
            // SAFETY: `count` live handles from the array.
            if unsafe { svcWaitSynchronization(&mut index, handles.as_ptr(), count as i32, nanos) } != 0 {
                break;
            }
        }
        CLOSING.store(false, Ordering::Release);
    }
    result.close_failed = CLOSE_FAILED.load(Ordering::Relaxed);
    result.untracked = UNTRACKED.load(Ordering::Relaxed);
    Some(result)
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

/// Added to the destination's name while it is moved aside.
const REPLACED_SUFFIX: &[u8] = b".replaced";

/// Horizon's file systems will not rename onto an existing file (libnx turns
/// `PathAlreadyExists` into EEXIST), where POSIX and std's `fs::rename` replace it: the settings
/// writer's temporary file failed every save after the first. Between two files this moves the
/// destination aside, renames, then deletes it, or moves it back if the rename failed. Unlike
/// POSIX it is not atomic: a title that dies in between leaves `<new>.replaced` and no `<new>`.
///
/// # Safety
/// As `rename`.
#[no_mangle]
pub unsafe extern "C" fn __wrap_rename(old: *const c_char, new: *const c_char) -> c_int {
    // SAFETY: the caller's arguments; errno is this thread's.
    unsafe {
        let rc = __real_rename(old, new);
        if rc == 0 || *__errno() != EEXIST {
            return rc;
        }
        match replace_file(old, new) {
            Ok(()) => 0,
            Err(errno) => {
                *__errno() = errno;
                -1
            }
        }
    }
}

/// `rename` onto an existing file. Errors are errno values.
///
/// # Safety
/// As `rename`.
unsafe fn replace_file(old: *const c_char, new: *const c_char) -> Result<(), c_int> {
    use std::ffi::{CStr, CString, OsStr};
    use std::os::unix::ffi::OsStrExt;

    let path = |c: &CStr| OsStr::from_bytes(c.to_bytes()).to_owned();
    let is_file = |c: &CStr| std::fs::symlink_metadata(path(c)).is_ok_and(|m| m.is_file());
    // SAFETY: the caller's NUL-terminated paths.
    let (old_c, new_c) = unsafe { (CStr::from_ptr(old), CStr::from_ptr(new)) };
    if !is_file(old_c) || !is_file(new_c) {
        return Err(EEXIST);
    }
    let mut aside = new_c.to_bytes().to_vec();
    aside.extend_from_slice(REPLACED_SUFFIX);
    let aside = CString::new(aside).map_err(|_| EEXIST)?;
    if std::fs::symlink_metadata(path(&aside)).is_ok() {
        return Err(EEXIST);
    }
    // SAFETY: NUL-terminated paths; errno is this thread's.
    unsafe {
        if __real_rename(new, aside.as_ptr()) != 0 {
            return Err(EEXIST);
        }
        if __real_rename(old, new) == 0 {
            let _ = std::fs::remove_file(path(&aside));
            return Ok(());
        }
        // Also the way out when `old` and `new` name the same file: it is now `aside`.
        let errno = *__errno();
        __real_rename(aside.as_ptr(), new);
        Err(errno)
    }
}

// ponytail: rmdir passes through; the ENOTEMPTY mapping comes when the app removes folders.

/// # Safety
/// As `rmdir`.
#[no_mangle]
pub unsafe extern "C" fn __wrap_rmdir(path: *const c_char) -> c_int {
    // SAFETY: the caller's argument.
    unsafe { __real_rmdir(path) }
}

/// The largest `SO_RCVBUF` the `bsd` service grants: 2 and 4 MiB fail with ENOBUFS.
const RCVBUF_MAX: c_int = 1 << 20;
/// Below this a step down is not worth taking: libnx's default reservation is 42240.
const RCVBUF_FLOOR: c_int = 64 << 10;

/// Core asks every UDP socket for a 32 MiB `SO_RCVBUF` once and keeps libnx's 41 KiB when the
/// `bsd` service refuses it, and a 41 KiB buffer drops shards of every burst larger than that
/// (WP3's first 720p run: 33 freezes in ten minutes, the bitrate held near 3 Mbps). A refused
/// receive size is asked again at 1 MiB, then halved down to 64 KiB.
///
/// # Safety
/// As `setsockopt`.
#[no_mangle]
pub unsafe extern "C" fn __wrap_setsockopt(
    fd: c_int,
    level: c_int,
    name: c_int,
    value: *const c_void,
    len: u32,
) -> c_int {
    // SAFETY: the caller's arguments; errno is this thread's.
    unsafe {
        let rc = __real_setsockopt(fd, level, name, value, len);
        if rc == 0 || level != SOL_SOCKET || name != SO_RCVBUF || *__errno() != ENOBUFS {
            return rc;
        }
        if value.is_null() || len as usize != core::mem::size_of::<c_int>() {
            return rc;
        }
        let mut ask = value.cast::<c_int>().read_unaligned().min(RCVBUF_MAX * 2) / 2;
        while ask >= RCVBUF_FLOOR {
            if __real_setsockopt(fd, level, name, (&raw const ask).cast(), len) == 0 {
                return 0;
            }
            ask /= 2;
        }
        *__errno() = ENOBUFS;
        -1
    }
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

/// newlib declares `posix_memalign` and devkitA64's libc does not define it; Granite's aligned
/// allocator (the `PyroWave` lane) calls it. newlib's `free` takes what `memalign` returns.
///
/// # Safety
/// `out` is writable.
#[no_mangle]
pub unsafe extern "C" fn posix_memalign(out: *mut *mut c_void, align: usize, size: usize) -> c_int {
    if !align.is_power_of_two() || align % core::mem::size_of::<*mut c_void>() != 0 {
        return EINVAL;
    }
    // SAFETY: newlib's allocator; `align` is a power of two.
    let p = unsafe { memalign(align, size) };
    if p.is_null() {
        return ENOMEM;
    }
    // SAFETY: the caller's pointer, writable.
    unsafe { *out = p };
    0
}

/// Horizon has no dynamic loader. libloading, which the `PyroWave` lane's dependencies link, names
/// these; every load fails, so nothing is ever looked up or closed.
#[no_mangle]
pub extern "C" fn dlopen(_file: *const c_char, _mode: c_int) -> *mut c_void {
    core::ptr::null_mut()
}

#[no_mangle]
pub extern "C" fn dlsym(_handle: *mut c_void, _name: *const c_char) -> *mut c_void {
    core::ptr::null_mut()
}

#[no_mangle]
pub extern "C" fn dlclose(_handle: *mut c_void) -> c_int {
    0
}

/// The reason for the last failed `dlopen` or `dlsym`, which is always the same.
#[no_mangle]
pub extern "C" fn dlerror() -> *const c_char {
    c"no dynamic loader on Horizon".as_ptr()
}
