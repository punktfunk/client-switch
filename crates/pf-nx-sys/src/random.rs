//! The OS-seeded `ChaCha` generator. Extern from `kernel/random.h`, libnx 4.12.0.

#[cfg(target_os = "horizon")]
extern "C" {
    fn randomGet(buf: *mut core::ffi::c_void, len: usize);
}

/// Fills `buf` with random bytes.
#[cfg(target_os = "horizon")]
pub fn fill(buf: &mut [u8]) {
    // SAFETY: `buf` is writable for its length.
    unsafe { randomGet(buf.as_mut_ptr().cast(), buf.len()) };
}
