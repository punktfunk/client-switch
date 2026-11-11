//! libnx's software framebuffer on the default window: the CPU-raster `Display` for the probe and
//! the emulator. Externs from `display/framebuffer.h` and `display/native_window.h`, libnx
//! 4.12.0.

/// `Framebuffer`, 80 bytes, aligned to 8. Only libnx reads it: `win`, an `NvMap`, two buffer
/// pointers, five `u32`s and `has_init`.
#[repr(C, align(8))]
pub struct RawFramebuffer(pub [u8; 80]);

/// `PIXEL_FORMAT_RGBA_8888` (display/types.h).
pub const PIXEL_FORMAT_RGBA_8888: u32 = 1;

#[cfg(target_os = "horizon")]
mod ffi {
    use super::RawFramebuffer;
    use core::ffi::c_void;

    extern "C" {
        pub fn framebufferCreate(
            fb: *mut RawFramebuffer,
            win: *mut c_void,
            width: u32,
            height: u32,
            format: u32,
            num_fbs: u32,
        ) -> u32;
        pub fn framebufferMakeLinear(fb: *mut RawFramebuffer) -> u32;
        pub fn framebufferClose(fb: *mut RawFramebuffer);
        pub fn framebufferBegin(fb: *mut RawFramebuffer, out_stride: *mut u32) -> *mut c_void;
        pub fn framebufferEnd(fb: *mut RawFramebuffer);
    }
}

/// A double-buffered linear RGBA8 framebuffer on the default window.
#[cfg(target_os = "horizon")]
pub struct Framebuffer {
    raw: Box<RawFramebuffer>,
    height: u32,
}

#[cfg(target_os = "horizon")]
impl Framebuffer {
    pub fn create(width: u32, height: u32) -> anyhow::Result<Self> {
        let mut fb = Self {
            raw: Box::new(RawFramebuffer([0; 80])),
            height,
        };
        // SAFETY: a zeroed Framebuffer for libnx to fill, and the default window, which lives
        // for the whole process. On failure `has_init` stays 0 and Drop's close is a no-op.
        let rc = unsafe {
            ffi::framebufferCreate(
                &mut *fb.raw,
                crate::vi::default_window(),
                width,
                height,
                PIXEL_FORMAT_RGBA_8888,
                2,
            )
        };
        if rc != 0 {
            return Err(crate::nx_error("create the framebuffer", rc));
        }
        // SAFETY: a framebuffer framebufferCreate initialised.
        let rc = unsafe { ffi::framebufferMakeLinear(&mut *fb.raw) };
        if rc != 0 {
            return Err(crate::nx_error("make the framebuffer linear", rc));
        }
        Ok(fb)
    }

    /// Dequeues the next buffer: the linear pixels, RGBA8, and the bytes per row. Pair each call
    /// with [`Framebuffer::end`].
    pub fn begin(&mut self) -> (&mut [u8], usize) {
        let mut stride = 0u32;
        // SAFETY: an initialised linear framebuffer; libnx aborts the title if dequeue fails.
        let p = unsafe { ffi::framebufferBegin(&mut *self.raw, &mut stride) };
        let len = stride as usize * self.height as usize;
        // SAFETY: with framebufferMakeLinear, `p` is the shadow buffer of `stride * height` bytes
        // libnx allocated, untouched by libnx until `end`; the borrow ties it to `self`.
        (
            unsafe { core::slice::from_raw_parts_mut(p.cast::<u8>(), len) },
            stride as usize,
        )
    }

    /// Converts the linear buffer to the panel's layout and presents it.
    pub fn end(&mut self) {
        // SAFETY: an initialised framebuffer.
        unsafe { ffi::framebufferEnd(&mut *self.raw) };
    }
}

#[cfg(target_os = "horizon")]
impl Drop for Framebuffer {
    fn drop(&mut self) {
        // SAFETY: closes what `create` made; a no-op when `has_init` is 0.
        unsafe { ffi::framebufferClose(&mut *self.raw) };
    }
}
