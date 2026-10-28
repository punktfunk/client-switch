//! NVDEC through libavcodec's `nvtegra` hwaccel, behind the C shim `csrc/pf_av.c`. The shim is
//! the only code that names libavcodec's structs; this side binds its six calls and `av_strerror`
//! (`libavutil/error.h`). Pictures come out as pitch-linear NV12 copied into the caller's planes.
//!
//! Off the console [`Decoder::open`] returns an error, so the workspace builds anywhere.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264 = 0,
    Hevc = 1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendStatus {
    /// The decoder copied the access unit.
    Taken,
    /// The decoder is full: receive a picture, then send the same unit again.
    Again,
}

/// `PfAvPictureMeta`: the picture's size and the pts its access unit was sent with.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PictureMeta {
    pub width: u32,
    pub height: u32,
    pub pts: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Pictures received since open.
    pub decoded: u64,
    /// Access units taken and not yet out as pictures.
    pub in_flight: u32,
    /// Microseconds from the last send to the last picture out.
    pub last_decode_us: u32,
    /// Microseconds of the last VIC transfer to NV12.
    pub last_transfer_us: u32,
}

/// `PfAvStats`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RawStats {
    pub decoded: u64,
    pub in_flight: u32,
    pub last_decode_us: u32,
    pub last_transfer_us: u32,
    pub reserved: u32,
}

/// The shim's own codes, outside libavcodec's `AVERROR` space (`csrc/pf_av.c`).
pub const PF_AV_NOT_NV12: i32 = -0x7066_0001;
pub const PF_AV_TOO_SMALL: i32 = -0x7066_0002;

#[cfg(target_os = "horizon")]
mod ffi {
    use super::{PictureMeta, RawStats};
    use core::ffi::{c_char, c_int};

    /// `PfAvDecoder`, owned by the shim.
    #[repr(C)]
    pub struct PfAvDecoder {
        _private: [u8; 0],
    }

    extern "C" {
        pub fn pf_av_open(codec: c_int, width: c_int, height: c_int, out: *mut *mut PfAvDecoder) -> c_int;
        pub fn pf_av_send(d: *mut PfAvDecoder, data: *const u8, len: c_int, pts: u64) -> c_int;
        pub fn pf_av_receive(
            d: *mut PfAvDecoder,
            y: *mut u8,
            y_stride: usize,
            y_len: usize,
            uv: *mut u8,
            uv_stride: usize,
            uv_len: usize,
            meta: *mut PictureMeta,
        ) -> c_int;
        pub fn pf_av_flush(d: *mut PfAvDecoder);
        pub fn pf_av_close(d: *mut PfAvDecoder);
        pub fn pf_av_stats(d: *const PfAvDecoder, out: *mut RawStats);
        pub fn av_strerror(errnum: c_int, errbuf: *mut c_char, errbuf_size: usize) -> c_int;
    }
}

#[cfg(target_os = "horizon")]
fn av_error(op: &str, code: i32) -> anyhow::Error {
    if code == PF_AV_NOT_NV12 {
        return anyhow::anyhow!("{op}: a software picture came out, so nvtegra did not take the stream");
    }
    let mut buf = [0 as core::ffi::c_char; 128];
    // SAFETY: `buf` is writable for its length; av_strerror NUL-terminates within it.
    unsafe { ffi::av_strerror(code, buf.as_mut_ptr(), buf.len()) };
    // SAFETY: NUL-terminated by av_strerror above.
    let msg = unsafe { core::ffi::CStr::from_ptr(buf.as_ptr()) }.to_string_lossy();
    anyhow::anyhow!("{op}: {msg} ({code})")
}

/// One hardware decoder. Used by one thread at a time.
pub struct Decoder {
    #[cfg(target_os = "horizon")]
    raw: core::ptr::NonNull<ffi::PfAvDecoder>,
    #[cfg(not(target_os = "horizon"))]
    never: Never,
}

#[cfg(not(target_os = "horizon"))]
enum Never {}

// SAFETY: the shim's decoder has no thread affinity; `&mut self` keeps it to one thread at once.
#[cfg(target_os = "horizon")]
unsafe impl Send for Decoder {}

#[cfg(target_os = "horizon")]
impl Decoder {
    /// Opens `codec` on NVDEC for a `width`×`height` stream.
    pub fn open(codec: Codec, width: u32, height: u32) -> anyhow::Result<Self> {
        let (w, h) = (i32::try_from(width)?, i32::try_from(height)?);
        let mut raw = core::ptr::null_mut();
        // SAFETY: `raw` is a valid out-pointer; the shim sets it only on success.
        let rc = unsafe { ffi::pf_av_open(codec as i32, w, h, &mut raw) };
        if rc < 0 {
            return Err(av_error(&format!("open the {codec:?} decoder"), rc));
        }
        let raw =
            core::ptr::NonNull::new(raw).ok_or_else(|| anyhow::anyhow!("open the {codec:?} decoder: no decoder"))?;
        Ok(Self { raw })
    }

    /// Hands the decoder one access unit; the shim copies it.
    pub fn send(&mut self, au: &[u8], pts: u64) -> anyhow::Result<SendStatus> {
        // An empty packet would start FFmpeg's drain.
        anyhow::ensure!(!au.is_empty(), "send an empty access unit");
        let len = i32::try_from(au.len())?;
        // SAFETY: `au` is readable for `len` bytes and copied before the call returns.
        let rc = unsafe { ffi::pf_av_send(self.raw.as_ptr(), au.as_ptr(), len, pts) };
        match rc {
            0 => Ok(SendStatus::Taken),
            1 => Ok(SendStatus::Again),
            _ => Err(av_error("send an access unit", rc)),
        }
    }

    /// Copies the next picture's NV12 planes into `y` and `uv`; `None` when none is ready.
    pub fn receive_into(
        &mut self,
        y: &mut [u8],
        y_stride: usize,
        uv: &mut [u8],
        uv_stride: usize,
    ) -> anyhow::Result<Option<PictureMeta>> {
        let mut meta = PictureMeta::default();
        // SAFETY: each plane pointer is writable for the length passed with it; the shim checks
        // every row against those lengths before it copies.
        let rc = unsafe {
            ffi::pf_av_receive(
                self.raw.as_ptr(),
                y.as_mut_ptr(),
                y_stride,
                y.len(),
                uv.as_mut_ptr(),
                uv_stride,
                uv.len(),
                &mut meta,
            )
        };
        match rc {
            1 => Ok(Some(meta)),
            0 => Ok(None),
            PF_AV_TOO_SMALL => Err(anyhow::anyhow!(
                "receive a picture: {}×{} does not fit planes of {} and {} bytes at strides {y_stride} and {uv_stride}",
                meta.width,
                meta.height,
                y.len(),
                uv.len()
            )),
            _ => Err(av_error("receive a picture", rc)),
        }
    }

    /// Drops every access unit and picture in the decoder.
    pub fn flush(&mut self) {
        // SAFETY: a live decoder.
        unsafe { ffi::pf_av_flush(self.raw.as_ptr()) };
    }

    pub fn stats(&self) -> Stats {
        let mut s = RawStats::default();
        // SAFETY: a live decoder and a valid out-pointer.
        unsafe { ffi::pf_av_stats(self.raw.as_ptr(), &mut s) };
        Stats {
            decoded: s.decoded,
            in_flight: s.in_flight,
            last_decode_us: s.last_decode_us,
            last_transfer_us: s.last_transfer_us,
        }
    }
}

#[cfg(target_os = "horizon")]
impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: `raw` came from pf_av_open and is closed once.
        unsafe { ffi::pf_av_close(self.raw.as_ptr()) };
    }
}

#[cfg(not(target_os = "horizon"))]
impl Decoder {
    pub fn open(codec: Codec, _width: u32, _height: u32) -> anyhow::Result<Self> {
        anyhow::bail!("open the {codec:?} decoder: NVDEC exists only on the console")
    }

    pub fn send(&mut self, _au: &[u8], _pts: u64) -> anyhow::Result<SendStatus> {
        match self.never {}
    }

    pub fn receive_into(
        &mut self,
        _y: &mut [u8],
        _y_stride: usize,
        _uv: &mut [u8],
        _uv_stride: usize,
    ) -> anyhow::Result<Option<PictureMeta>> {
        match self.never {}
    }

    pub fn flush(&mut self) {
        match self.never {}
    }

    pub fn stats(&self) -> Stats {
        match self.never {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "horizon"))]
    #[test]
    fn open_fails_off_the_console() {
        let err = Decoder::open(Codec::Hevc, 1920, 1080).err().unwrap();
        assert_eq!(
            err.to_string(),
            "open the Hevc decoder: NVDEC exists only on the console"
        );
    }

    #[test]
    fn shim_structs_match_pf_av_c() {
        use core::mem::{offset_of, size_of};
        // PfAvPictureMeta: u32 width, u32 height, u64 pts → 16.
        assert_eq!((size_of::<PictureMeta>(), offset_of!(PictureMeta, pts)), (16, 8));
        // PfAvStats: u64 decoded, four u32 → 24.
        assert_eq!(
            (size_of::<RawStats>(), offset_of!(RawStats, last_transfer_us)),
            (24, 16)
        );
        assert_eq!(Codec::Hevc as i32, 1);
    }
}
