//! NVDEC through libavcodec's `nvtegra` hwaccel, behind the C shim `csrc/pf_av.c`. The shim is
//! the only code that names libavcodec's structs; this side binds its six calls and `av_strerror`
//! (`libavutil/error.h`). Pictures come out as pitch-linear NV12 copied into the caller's planes.
//!
//! On Linux the same shim decodes in software against the system's libavcodec, for the dev box;
//! elsewhere [`Decoder::open`] returns an error, so the workspace builds anywhere.

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
    /// Microseconds of the last transfer to NV12 (the VIC's, or the CPU's detiling).
    pub last_transfer_us: u32,
    /// Microseconds of the last copy out of libavutil's frame; 0 when there was none.
    pub last_copy_us: u32,
    pub route: Route,
    /// Why transfers stopped going straight into the caller's planes, an `AVERROR`; 0 while
    /// they do.
    pub direct_err: i32,
}

/// How the last picture reached the caller's planes (`PF_AV_ROUTE_*`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Route {
    /// A software decoder's picture, copied (the dev box).
    #[default]
    Software,
    /// The VIC into libavutil's frame, then copied.
    VicThenCopy,
    /// libavutil's frame was not 256-byte aligned, so the hwcontext detiled on the CPU, then
    /// copied.
    CpuThenCopy,
    /// The VIC straight into the caller's planes.
    Vic,
    /// Detiled on the CPU straight into the caller's planes.
    Cpu,
}

impl Route {
    fn from_raw(raw: u32) -> Self {
        match raw {
            1 => Self::VicThenCopy,
            2 => Self::CpuThenCopy,
            3 => Self::Vic,
            4 => Self::Cpu,
            _ => Self::Software,
        }
    }

    /// For the log.
    pub fn name(self) -> &'static str {
        match self {
            Self::Software => "software, copied",
            Self::VicThenCopy => "VIC, copied",
            Self::CpuThenCopy => "CPU detile, copied",
            Self::Vic => "VIC straight",
            Self::Cpu => "CPU detile straight",
        }
    }
}

/// `PfAvStats`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RawStats {
    pub decoded: u64,
    pub in_flight: u32,
    pub last_decode_us: u32,
    pub last_transfer_us: u32,
    pub last_copy_us: u32,
    pub route: u32,
    pub direct_err: i32,
}

/// The shim's own codes, outside libavcodec's `AVERROR` space (`csrc/pf_av.c`).
pub const PF_AV_NOT_NV12: i32 = -0x7066_0001;
pub const PF_AV_TOO_SMALL: i32 = -0x7066_0002;

#[cfg(pf_av)]
mod ffi {
    use super::{PictureMeta, RawStats};
    use core::ffi::{c_char, c_int};

    /// `PfAvDecoder`, owned by the shim.
    #[repr(C)]
    pub struct PfAvDecoder {
        _private: [u8; 0],
    }

    extern "C" {
        pub fn pf_av_open(
            codec: c_int,
            width: c_int,
            height: c_int,
            direct: c_int,
            out: *mut *mut PfAvDecoder,
        ) -> c_int;
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

#[cfg(pf_av)]
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

/// One decoder: NVDEC on the console, software on a Linux dev box. Used by one thread at a time.
pub struct Decoder {
    #[cfg(pf_av)]
    raw: core::ptr::NonNull<ffi::PfAvDecoder>,
    #[cfg(not(pf_av))]
    never: Never,
}

#[cfg(not(pf_av))]
enum Never {}

// SAFETY: the shim's decoder has no thread affinity; `&mut self` keeps it to one thread at once.
#[cfg(pf_av)]
unsafe impl Send for Decoder {}

#[cfg(pf_av)]
impl Decoder {
    /// Opens `codec` for a `width`×`height` stream. `straight` has the VIC write into the
    /// caller's planes ([`Route::Vic`]) where it can; otherwise every picture goes through
    /// libavutil's frame and a copy ([`Route::VicThenCopy`]).
    pub fn open(codec: Codec, width: u32, height: u32, straight: bool) -> anyhow::Result<Self> {
        let (w, h) = (i32::try_from(width)?, i32::try_from(height)?);
        let mut raw = core::ptr::null_mut();
        // SAFETY: `raw` is a valid out-pointer; the shim sets it only on success.
        let rc = unsafe { ffi::pf_av_open(codec as i32, w, h, i32::from(straight), &mut raw) };
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
            last_copy_us: s.last_copy_us,
            route: Route::from_raw(s.route),
            direct_err: s.direct_err,
        }
    }
}

#[cfg(pf_av)]
impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: `raw` came from pf_av_open and is closed once.
        unsafe { ffi::pf_av_close(self.raw.as_ptr()) };
    }
}

#[cfg(not(pf_av))]
impl Decoder {
    pub fn open(codec: Codec, _width: u32, _height: u32, _straight: bool) -> anyhow::Result<Self> {
        anyhow::bail!("open the {codec:?} decoder: no decoder on this box (NVDEC on the console, libavcodec on Linux)")
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

    #[cfg(not(pf_av))]
    #[test]
    fn open_fails_off_the_console() {
        let err = Decoder::open(Codec::Hevc, 1920, 1080, false).err().unwrap();
        assert_eq!(
            err.to_string(),
            "open the Hevc decoder: no decoder on this box (NVDEC on the console, libavcodec on Linux)"
        );
    }

    #[cfg(all(pf_av, target_os = "linux"))]
    #[test]
    fn a_dev_box_opens_both_codecs_in_software() {
        for codec in [Codec::H264, Codec::Hevc] {
            let mut d = Decoder::open(codec, 1920, 1080, false).unwrap();
            assert_eq!(d.stats(), Stats::default());
            d.flush();
        }
    }

    #[test]
    fn shim_structs_match_pf_av_c() {
        use core::mem::{offset_of, size_of};
        // PfAvPictureMeta: u32 width, u32 height, u64 pts → 16.
        assert_eq!((size_of::<PictureMeta>(), offset_of!(PictureMeta, pts)), (16, 8));
        // PfAvStats: u64 decoded, five u32 and an i32 → 32.
        assert_eq!(
            (
                size_of::<RawStats>(),
                offset_of!(RawStats, last_transfer_us),
                offset_of!(RawStats, direct_err)
            ),
            (32, 16, 28)
        );
        assert_eq!(Codec::Hevc as i32, 1);
    }
}
