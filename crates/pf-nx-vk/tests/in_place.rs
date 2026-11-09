//! The in-place route on lavapipe: a picture laid out as NVDEC lays it, 16Bx2 block-linear in
//! 4 KiB-aligned host memory, imported and drawn by the tiled pass, against the same picture
//! through the NV12 upload. Of the console's route only NVDEC is missing.
#![cfg(target_os = "linux")]

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::sync::atomic::AtomicI64;
use std::sync::Arc;

use pf_nx_app::platform::{Display, Fit, StreamSpec, VideoE2e};
use pf_nx_vk::in_place::block_linear;
use pf_nx_vk::{Check, GpuDevice, InPlace, Nv12Upload, Presenter, Target, TiledSurface, VideoSlot};
use punktfunk_core::quic;

const W: u32 = 200;
const H: u32 = 120;
/// NVDEC's pitch for `W` (64-byte aligned) and its plane height (32-row aligned).
const PITCH: u32 = 256;
const CHROMA_OFFSET: u32 = PITCH * 128;
const SIZE: usize = (CHROMA_OFFSET + PITCH * 64) as usize;

fn luma(x: u32, y: u32) -> u8 {
    ((x * 7 + y * 13 + (x * y) % 31) & 0xff) as u8
}

fn chroma(x: u32, y: u32) -> [u8; 2] {
    [((x * 5 + y * 3 + 40) & 0xff) as u8, (((x * 11) ^ (y * 7)) & 0xff) as u8]
}

fn spec() -> StreamSpec {
    StreamSpec {
        codec: quic::CODEC_HEVC,
        width: W,
        height: H,
        refresh_hz: 60,
        color: quic::ColorInfo {
            primaries: 1,
            transfer: 1,
            matrix: 1,
            full_range: 0,
        },
        bit_depth: 8,
        chroma444: false,
        shard_payload: 1200,
        smoothing: 0,
        hud: Arc::new(punktfunk_core::hud::Stats::new(Arc::new(AtomicI64::new(0)))),
        video_e2e: VideoE2e::default(),
    }
}

/// The picture pitch-linear, rows `stride` bytes apart: luma, then interleaved `CbCr`.
fn linear(stride: usize) -> (Vec<u8>, Vec<u8>) {
    let mut y = vec![0; stride * H as usize];
    let mut uv = vec![0; stride * H.div_ceil(2) as usize];
    for r in 0..H {
        for c in 0..W {
            y[r as usize * stride + c as usize] = luma(c, r);
        }
    }
    for r in 0..H.div_ceil(2) {
        for c in 0..W / 2 {
            let at = r as usize * stride + 2 * c as usize;
            uv[at..at + 2].copy_from_slice(&chroma(c, r));
        }
    }
    (y, uv)
}

/// 4 KiB-aligned host memory, as nvtegra allocates a surface.
struct Surface(*mut u8);

impl Surface {
    fn layout() -> Layout {
        Layout::from_size_align(SIZE, 4096).unwrap()
    }

    /// The picture block-linear, or written pitch-linear where `tiled` is false.
    fn new(tiled: bool) -> Self {
        // SAFETY: a non-zero layout.
        let p = unsafe { alloc_zeroed(Self::layout()) };
        assert!(!p.is_null());
        // SAFETY: `p` holds SIZE bytes; every offset below stays inside them.
        let mem = unsafe { std::slice::from_raw_parts_mut(p, SIZE) };
        let at = |x: u32, y: u32| if tiled { block_linear(x, y, PITCH) } else { y * PITCH + x } as usize;
        for r in 0..H {
            for c in 0..W {
                mem[at(c, r)] = luma(c, r);
            }
        }
        for r in 0..H.div_ceil(2) {
            for c in 0..W / 2 {
                let [u, v] = chroma(c, r);
                mem[CHROMA_OFFSET as usize + at(2 * c, r)] = u;
                mem[CHROMA_OFFSET as usize + at(2 * c + 1, r)] = v;
            }
        }
        Self(p)
    }

    fn tiled(&self) -> TiledSurface {
        TiledSurface {
            base: self.0,
            size: SIZE,
            pitch: PITCH,
            chroma_offset: CHROMA_OFFSET,
            width: W,
            height: H,
            pool: 0,
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` with this layout.
        unsafe { dealloc(self.0, Self::layout()) };
    }
}

fn frame(presenter: &mut Presenter) -> Vec<u8> {
    presenter.frame(&mut |_| {}).expect("frame");
    presenter.read_back().expect("read back").expect("an image").pixels
}

fn max_diff(a: &[u8], b: &[u8]) -> u8 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0)
}

/// The device, or `None` (said) on a box without Vulkan.
fn device(width: u32, height: u32) -> Option<Arc<GpuDevice>> {
    // SAFETY: loads the system's Vulkan loader; nothing else in the test holds it.
    let entry = unsafe { ash::Entry::load() }
        .inspect_err(|e| eprintln!("no Vulkan loader: skipped ({e})"))
        .ok()?;
    let dev = GpuDevice::new(entry, Target::Offscreen { width, height })
        .inspect_err(|e| eprintln!("no Vulkan device: skipped ({e:#})"))
        .ok()?;
    assert!(dev.host_import, "lavapipe imports host memory");
    Some(Arc::new(dev))
}

/// The picture drawn onto a `width`×`height` output through the upload, in place, and in place
/// from a surface written pitch-linear.
fn draw_both(width: u32, height: u32) -> Option<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let dev = device(width, height)?;
    let slot = Arc::new(VideoSlot::default());
    let mut presenter = Presenter::new(dev.clone(), slot.clone()).expect("presenter");
    presenter.set_stream(true, Fit::Contain);
    let spec = spec();
    let mut upload = Nv12Upload::new(dev.clone(), slot.clone(), &spec).expect("upload");
    let mut lane = InPlace::new(dev, slot, &spec).expect("in-place lane");
    let (surface, linear_surface) = (Surface::new(true), Surface::new(false));

    let planes = upload.stage().expect("stage");
    let (y, uv) = linear(planes.y_stride);
    planes.y[..y.len()].copy_from_slice(&y);
    planes.uv[..uv.len()].copy_from_slice(&uv);
    assert!(upload.commit(W, H, 0, None).expect("commit"));
    let want = frame(&mut presenter);
    lane.publish(&surface.tiled(), 0, None, Box::new(())).expect("publish");
    let got = frame(&mut presenter);
    lane.publish(&linear_surface.tiled(), 0, None, Box::new(()))
        .expect("publish");
    let wrong = frame(&mut presenter);

    // The imports go before the memory they wrap.
    presenter.set_stream(false, Fit::Contain);
    drop(presenter);
    drop(lane);
    Some((want, got, wrong))
}

// The upload's sampler filters in 8-bit fixed point, the tiled pass in float: about 1 LSB of
// chroma, which the CSC doubles. Neighbouring texels here differ by up to 255, so a wrong
// address or siting costs tens.
#[test]
fn tiled_pass_draws_what_the_upload_draws_at_one_to_one() {
    let Some((want, got, wrong)) = draw_both(W, H) else {
        return;
    };
    assert!(max_diff(&want, &got) <= 3, "differs by {}", max_diff(&want, &got));
    assert!(max_diff(&want, &wrong) > 32, "a pitch-linear surface passes");
}

/// Scaled, the sampler also rounds its coordinates, so luma and chroma both carry its error.
#[test]
fn tiled_pass_draws_what_the_upload_draws_scaled() {
    let Some((want, got, _)) = draw_both(W * 3 / 2, H * 3 / 2) else {
        return;
    };
    assert!(max_diff(&want, &got) <= 8, "differs by {}", max_diff(&want, &got));
}

/// The picture drawn in place onto a `width`×`height` output.
fn draw_in_place(width: u32, height: u32) -> Option<Vec<u8>> {
    let dev = device(width, height)?;
    let slot = Arc::new(VideoSlot::default());
    let mut presenter = Presenter::new(dev.clone(), slot.clone()).expect("presenter");
    presenter.set_stream(true, Fit::Contain);
    let mut lane = InPlace::new(dev, slot, &spec()).expect("in-place lane");
    let surface = Surface::new(true);
    lane.publish(&surface.tiled(), 0, None, Box::new(())).expect("publish");
    let got = frame(&mut presenter);
    presenter.set_stream(false, Fit::Contain);
    drop(presenter);
    drop(lane);
    Some(got)
}

/// A picture the size of its output reads luma once a pixel. Two columns wider, the same picture
/// sits one pixel in, its texels still on pixel centres, and goes through the filter: both draw
/// the same pixels, up to the float rounding of the interpolated coordinate.
#[test]
fn one_to_one_reads_what_the_filter_reads() {
    let (Some(exact), Some(wide)) = (draw_in_place(W, H), draw_in_place(W + 2, H)) else {
        return;
    };
    let row = (W * 4) as usize;
    let inner: Vec<u8> = wide
        .chunks_exact(row + 8)
        .flat_map(|r| r[4..4 + row].iter().copied())
        .collect();
    let differ = exact.iter().zip(&inner).filter(|(a, b)| a != b).count();
    eprintln!("one to one: {differ} of {} bytes differ from the filter", exact.len());
    assert!(max_diff(&exact, &inner) <= 1, "differs by {}", max_diff(&exact, &inner));
}

/// The check reads a tiled surface back as the VIC would have copied it, and finds one written
/// pitch-linear wrong.
#[test]
fn check_tells_the_layouts_apart() {
    let Some(dev) = device(W, H) else { return };
    let slot = Arc::new(VideoSlot::default());
    let mut lane = InPlace::new(dev, slot, &spec()).expect("in-place lane");
    let (tiled, linear_surface) = (Surface::new(true), Surface::new(false));
    let stride = PITCH as usize;
    let (y, uv) = linear(stride);
    let verdict = lane.check(&tiled.tiled(), &y, stride, &uv, stride).expect("check");
    assert_eq!(verdict, Check::Match { atoms: 512, differ: 0 });
    let verdict = lane
        .check(&linear_surface.tiled(), &y, stride, &uv, stride)
        .expect("check");
    assert!(matches!(verdict, Check::Differ { .. }), "{verdict:?}");
    drop(lane);
}
