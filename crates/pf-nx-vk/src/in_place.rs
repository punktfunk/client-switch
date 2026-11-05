//! Pictures sampled where NVDEC wrote them. A decoder surface is imported once
//! (`VK_EXT_external_memory_host`) as a buffer, with an R8 view of its luma plane and an RG8 view
//! of its `CbCr` plane, and the tiled CSC pass detiles the 16Bx2 block-linear layout as it samples.
//! No VIC transfer, no copy, no upload: a picture is published once NVDEC is done with it.
//!
//! NVDEC cycles through a few surfaces, so imports are kept, by address, while the lane lives.
//! [`InPlace::check`] reads one picture's surface back through the GPU at the addresses the shader
//! samples and compares it with the VIC's copy of the same picture: a layout or an import the
//! console does not honour shows there, before any picture goes out in place.

use std::any::Any;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{ensure, Context, Result};
use ash::vk;
use pf_client_video::video_color::ColorDesc;
use pf_nx_app::platform::StreamSpec;

use crate::device::GpuDevice;
use crate::picture::{Picture, Planes, VideoSlot};

/// Surfaces kept imported: NVDEC's references and the pictures in flight, a dozen at the deepest
/// smoothing budget.
const MAX_IMPORTS: usize = 32;
/// A check samples this many rows by this many 16-byte atoms in each plane.
const CHECK_GRID: u32 = 16;
/// A byte read back within this of the VIC's copy is the same sample.
const CHECK_TOLERANCE: u8 = 2;
/// Luma samples spread less than this cannot tell one layout from another.
const CHECK_MIN_SPREAD: u8 = 32;

/// A decoder surface as the lane hands it over: NV12, both planes 16Bx2 block-linear.
#[derive(Clone, Copy, Debug)]
pub struct TiledSurface {
    /// The surface's memory, 4 KiB aligned, and its length, a 4 KiB multiple.
    pub base: *const u8,
    pub size: usize,
    /// Bytes a row of either plane, a multiple of 64.
    pub pitch: u32,
    pub chroma_offset: u32,
    pub width: u32,
    pub height: u32,
    /// The decoder's surface pool. A new one may reuse an older one's freed addresses.
    pub pool: u32,
}

/// The byte at column `x`, row `y` of a 16Bx2 block-linear plane `pitch` bytes wide with two
/// GOBs (16 rows) to a block. `shaders/nv12_tiled.frag` has the same function.
pub fn block_linear(x: u32, y: u32, pitch: u32) -> u32 {
    (y >> 4) * (pitch << 4)
        + (x >> 6) * 1024
        + ((y >> 3) & 1) * 512
        + ((x >> 5) & 1) * 256
        + ((y >> 1) & 3) * 64
        + ((x >> 4) & 1) * 32
        + (y & 1) * 16
        + (x & 15)
}

/// What a check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// The GPU read the sampled atoms as the VIC wrote them, but for `differ` of `atoms`.
    Match { atoms: usize, differ: usize },
    /// Too many of the sampled atoms came back otherwise: not a layout to sample in place.
    Differ { atoms: usize, differ: usize },
    /// Too little detail to tell layouts apart; a later picture decides.
    Flat,
}

/// One surface's import: the buffer over its memory and the plane views the shader reads.
struct Import {
    dev: Arc<GpuDevice>,
    base: usize,
    size: usize,
    pool: u32,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    views: [vk::BufferView; 2],
}

impl Drop for Import {
    fn drop(&mut self) {
        // SAFETY: pictures hold an import until the frame that sampled it has finished, and a
        // check waits its copy, so nothing in flight reads it; null handles are no-ops.
        unsafe {
            for view in self.views {
                self.dev.device.destroy_buffer_view(view, None);
            }
            self.dev.device.destroy_buffer(self.buffer, None);
            self.dev.device.free_memory(self.memory, None);
        }
    }
}

/// The check's copy out of a surface into memory the CPU reads.
struct Readback {
    dev: Arc<GpuDevice>,
    pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    ptr: *const u8,
}

impl Drop for Readback {
    fn drop(&mut self) {
        // SAFETY: every check waits its fence, so the copy is done; the handles are this one's.
        unsafe {
            self.dev.device.destroy_fence(self.fence, None);
            self.dev.device.destroy_command_pool(self.pool, None);
            self.dev.device.destroy_buffer(self.buffer, None);
            self.dev.device.free_memory(self.memory, None);
        }
    }
}

/// Decoder surfaces published to the presenter's slot as they are.
pub struct InPlace {
    dev: Arc<GpuDevice>,
    slot: Arc<VideoSlot>,
    color: ColorDesc,
    hud: Arc<punktfunk_core::hud::Stats>,
    e2e: pf_nx_app::platform::VideoE2e,
    host: ash::ext::external_memory_host::Device,
    /// Oldest first.
    imports: Vec<Arc<Import>>,
    readback: Option<Readback>,
}

// SAFETY: the read-back pointer maps memory this value owns, read only by `check` under
// `&mut self`; the rest is handles and `Arc`s that are `Send`.
unsafe impl Send for InPlace {}

impl InPlace {
    /// For `spec`'s stream on `dev`, publishing to `slot`. Fails where the device cannot import
    /// host memory or read the planes as texel buffers, or for an HDR stream.
    pub fn new(dev: Arc<GpuDevice>, slot: Arc<VideoSlot>, spec: &StreamSpec) -> Result<Self> {
        ensure!(dev.host_import, "the device cannot import host memory");
        let color = crate::picture::color_of(spec);
        ensure!(
            !color.is_pq(),
            "an HDR stream needs the tonemap, which the tiled pass leaves out"
        );
        for format in [vk::Format::R8_UNORM, vk::Format::R8G8_UNORM] {
            // SAFETY: read-only query on a live physical device.
            let props = unsafe { dev.instance.get_physical_device_format_properties(dev.pdev, format) };
            ensure!(
                props
                    .buffer_features
                    .contains(vk::FormatFeatureFlags::UNIFORM_TEXEL_BUFFER),
                "the device has no {format:?} texel buffers"
            );
        }
        let host = ash::ext::external_memory_host::Device::new(&dev.instance, &dev.device);
        Ok(Self {
            dev,
            slot,
            color,
            hud: spec.hud.clone(),
            e2e: spec.video_e2e.clone(),
            host,
            imports: Vec::new(),
            readback: None,
        })
    }

    /// Queues the picture in `s` on the slot, due on glass at `due`. `hold` keeps the surface
    /// from being decoded into again; the picture keeps it until its last frame has sampled it.
    pub fn publish(
        &mut self,
        s: &TiledSurface,
        pts_ns: u64,
        due: Option<Instant>,
        hold: Box<dyn Any + Send + Sync>,
    ) -> Result<()> {
        let import = self.import(s)?;
        self.slot.publish(Picture {
            planes: Planes::Tiled {
                views: import.views,
                pitch: s.pitch,
            },
            layout: vk::ImageLayout::UNDEFINED,
            width: s.width,
            height: s.height,
            color: self.color,
            depth: 8,
            msb_packed: false,
            uv_scale: [1.0, 1.0],
            decoded_at: Instant::now(),
            due,
            pts_ns,
            decoded_ns: u64::try_from(punktfunk_core::client::now_realtime_ns()).unwrap_or(0),
            turn: None,
            hud: Some(self.hud.clone()),
            e2e: Some(self.e2e.clone()),
            // The import drops first: the GPU's mapping ends before NVDEC may reuse the surface.
            hold: Box::new((import, hold)),
        });
        Ok(())
    }

    /// Reads `s` back through the GPU at the addresses the tiled pass samples and compares it with
    /// the same picture as the VIC wrote it, pitch-linear, into `y` and `uv`.
    pub fn check(&mut self, s: &TiledSurface, y: &[u8], y_stride: usize, uv: &[u8], uv_stride: usize) -> Result<Check> {
        ensure!(
            s.width >= 32 && s.height >= 2,
            "a {}x{} picture is too small to check",
            s.width,
            s.height
        );
        let atoms = atoms(s, y_stride, uv_stride);
        let import = self.import(s)?;
        let bytes = atoms.len() * 16;
        if self.readback.is_none() {
            self.readback = Some(readback(&self.dev, bytes as u64)?);
        }
        let rb = self.readback.as_ref().expect("made just above");
        let regions: Vec<vk::BufferCopy> = atoms
            .iter()
            .enumerate()
            .map(|(i, a)| {
                vk::BufferCopy::default()
                    .src_offset(a.surface)
                    .dst_offset(i as u64 * 16)
                    .size(16)
            })
            .collect();
        let dev = &self.dev;
        // SAFETY: the command buffer and fence are the read-back's and idle (each check waits
        // them); the copy reads the import, which stays alive in the cache, into the read-back.
        unsafe {
            dev.device
                .reset_command_buffer(rb.cmd, vk::CommandBufferResetFlags::empty())?;
            dev.device.begin_command_buffer(
                rb.cmd,
                &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            crate::util::memory_barrier(
                dev,
                rb.cmd,
                (
                    vk::PipelineStageFlags2::HOST | vk::PipelineStageFlags2::ALL_COMMANDS,
                    vk::AccessFlags2::HOST_WRITE | vk::AccessFlags2::MEMORY_WRITE,
                ),
                (vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_READ),
            );
            dev.device.cmd_copy_buffer(rb.cmd, import.buffer, rb.buffer, &regions);
            crate::util::memory_barrier(
                dev,
                rb.cmd,
                (vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_WRITE),
                (vk::PipelineStageFlags2::HOST, vk::AccessFlags2::HOST_READ),
            );
            dev.device.end_command_buffer(rb.cmd)?;
            let cmds = [rb.cmd];
            {
                let _q = dev.vkd.queue_lock.guard();
                dev.device
                    .queue_submit(dev.queue, &[vk::SubmitInfo::default().command_buffers(&cmds)], rb.fence)?;
            }
            let waited = dev.device.wait_for_fences(&[rb.fence], true, 1_000_000_000);
            dev.device.reset_fences(&[rb.fence])?;
            waited.context("check read-back")?;
        }
        // SAFETY: `ptr` maps `bytes` of host-coherent memory the copy above finished writing.
        let got = unsafe { std::slice::from_raw_parts(rb.ptr, bytes) };
        let mut differ = 0;
        let (mut lo, mut hi) = (u8::MAX, u8::MIN);
        for (i, a) in atoms.iter().enumerate() {
            let plane = if a.luma { y } else { uv };
            let want = plane
                .get(a.linear..a.linear + 16)
                .context("the VIC's copy is smaller than the picture")?;
            if a.luma {
                lo = want.iter().fold(lo, |m, &b| m.min(b));
                hi = want.iter().fold(hi, |m, &b| m.max(b));
            }
            let read = &got[i * 16..i * 16 + 16];
            differ += usize::from(read.iter().zip(want).any(|(g, w)| g.abs_diff(*w) > CHECK_TOLERANCE));
        }
        if hi.saturating_sub(lo) < CHECK_MIN_SPREAD {
            return Ok(Check::Flat);
        }
        // A wrong layout or import reads back nearly every atom wrong; the VIC may round a few.
        let atoms = atoms.len();
        Ok(if differ * 32 <= atoms {
            Check::Match { atoms, differ }
        } else {
            Check::Differ { atoms, differ }
        })
    }

    /// The import of `s`, made on first sight. Imports of an older pool go, those still on
    /// screen with their pictures; past [`MAX_IMPORTS`] the oldest that no picture holds goes.
    fn import(&mut self, s: &TiledSurface) -> Result<Arc<Import>> {
        self.imports.retain(|i| i.pool == s.pool);
        let base = s.base as usize;
        if let Some(import) = self.imports.iter().find(|i| i.base == base && i.size == s.size) {
            return Ok(import.clone());
        }
        if self.imports.len() >= MAX_IMPORTS {
            if let Some(at) = self.imports.iter().position(|i| Arc::strong_count(i) == 1) {
                self.imports.remove(at);
            }
        }
        let import = Arc::new(make_import(&self.dev, &self.host, s)?);
        self.imports.push(import.clone());
        Ok(import)
    }
}

/// Imports `s`'s memory as a buffer with its two plane views.
fn make_import(
    dev: &Arc<GpuDevice>,
    host: &ash::ext::external_memory_host::Device,
    s: &TiledSurface,
) -> Result<Import> {
    // SAFETY: read-only query on a live physical device.
    let limits = unsafe { dev.instance.get_physical_device_properties(dev.pdev) }.limits;
    let (size, chroma) = (s.size as u64, u64::from(s.chroma_offset));
    ensure!(
        chroma > 0 && chroma < size && chroma % limits.min_texel_buffer_offset_alignment.max(1) == 0,
        "the CbCr plane at {chroma} of {size} bytes cannot start a texel buffer"
    );
    ensure!(
        size <= u64::from(limits.max_texel_buffer_elements),
        "a {size}-byte surface outgrows the device's texel buffers"
    );
    // Handles are filled in place: a failed step drops the import, which destroys what was made.
    let mut import = Import {
        dev: dev.clone(),
        base: s.base as usize,
        size: s.size,
        pool: s.pool,
        buffer: vk::Buffer::null(),
        memory: vk::DeviceMemory::null(),
        views: [vk::BufferView::null(); 2],
    };
    let handle = vk::ExternalMemoryHandleTypeFlags::HOST_ALLOCATION_EXT;
    let mut external = vk::ExternalMemoryBufferCreateInfo::default().handle_types(handle);
    // SAFETY: the builders are locals that outlive the calls; `s.base` is 4 KiB aligned host
    // memory the decoder keeps for longer than the import, which the picture holds before it.
    unsafe {
        import.buffer = dev.device.create_buffer(
            &vk::BufferCreateInfo::default()
                .size(size)
                .usage(vk::BufferUsageFlags::UNIFORM_TEXEL_BUFFER | vk::BufferUsageFlags::TRANSFER_SRC)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .push_next(&mut external),
            None,
        )?;
        let mut props = vk::MemoryHostPointerPropertiesEXT::default();
        (host.fp().get_memory_host_pointer_properties_ext)(dev.device.handle(), handle, s.base.cast(), &mut props)
            .result()
            .context("host pointer properties")?;
        let req = dev.device.get_buffer_memory_requirements(import.buffer);
        ensure!(
            req.size <= size,
            "the buffer wants {} bytes of a {size}-byte surface",
            req.size
        );
        let bits = props.memory_type_bits & req.memory_type_bits;
        // Cached keeps the decoder's pages as they are; a coherent import would make them uncached.
        let ty = dev
            .memory_type(bits, vk::MemoryPropertyFlags::HOST_CACHED)
            .or_else(|| dev.memory_type(bits, vk::MemoryPropertyFlags::empty()))
            .context("no memory type takes the decoder's surface")?;
        let mut from_host = vk::ImportMemoryHostPointerInfoEXT::default()
            .handle_type(handle)
            .host_pointer(s.base.cast_mut().cast());
        import.memory = dev
            .device
            .allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(size)
                    .memory_type_index(ty)
                    .push_next(&mut from_host),
                None,
            )
            .context("import the decoder's surface")?;
        dev.device.bind_buffer_memory(import.buffer, import.memory, 0)?;
        for (i, (format, offset, range)) in [
            (vk::Format::R8_UNORM, 0, chroma),
            (vk::Format::R8G8_UNORM, chroma, (size - chroma) & !1),
        ]
        .into_iter()
        .enumerate()
        {
            import.views[i] = dev.device.create_buffer_view(
                &vk::BufferViewCreateInfo::default()
                    .buffer(import.buffer)
                    .format(format)
                    .offset(offset)
                    .range(range),
                None,
            )?;
        }
    }
    Ok(import)
}

/// The read-back for a check of `bytes`, with its command buffer and fence.
fn readback(dev: &Arc<GpuDevice>, bytes: u64) -> Result<Readback> {
    let (buffer, memory, ptr) = crate::util::host_buffer(dev, bytes, vk::BufferUsageFlags::TRANSFER_DST)?;
    // Filled in place: a failed step drops the read-back, which destroys what was made.
    let mut rb = Readback {
        dev: dev.clone(),
        pool: vk::CommandPool::null(),
        cmd: vk::CommandBuffer::null(),
        fence: vk::Fence::null(),
        buffer,
        memory,
        ptr: ptr.cast_const(),
    };
    // SAFETY: the builders are locals; the objects are the read-back's, destroyed in its Drop.
    unsafe {
        rb.pool = dev.device.create_command_pool(
            &vk::CommandPoolCreateInfo::default()
                .queue_family_index(dev.qf)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
            None,
        )?;
        rb.cmd = dev.device.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(rb.pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1),
        )?[0];
        rb.fence = dev.device.create_fence(&vk::FenceCreateInfo::default(), None)?;
    }
    Ok(rb)
}

/// One 16-byte atom of a check: its offset in the surface, and where the same bytes sit in the
/// pitch-linear copy's luma or `CbCr` plane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Atom {
    surface: u64,
    luma: bool,
    linear: usize,
}

/// A [`CHECK_GRID`]-square grid of atoms over each plane, its rows and columns spread from edge
/// to edge, so every bit of the block-linear address takes both values.
fn atoms(s: &TiledSurface, y_stride: usize, uv_stride: usize) -> Vec<Atom> {
    let mut out = Vec::with_capacity(2 * (CHECK_GRID * CHECK_GRID) as usize);
    let planes = [
        (true, s.width, s.height, 0, y_stride),
        (
            false,
            s.width.next_multiple_of(2),
            s.height.div_ceil(2),
            s.chroma_offset,
            uv_stride,
        ),
    ];
    for (luma, row_bytes, rows, base, stride) in planes {
        for i in 0..CHECK_GRID {
            let y = i * (rows - 1) / (CHECK_GRID - 1);
            for j in 0..CHECK_GRID {
                let x = (j * (row_bytes - 16) / (CHECK_GRID - 1)) & !15;
                out.push(Atom {
                    surface: u64::from(base + block_linear(x, y, s.pitch)),
                    luma,
                    linear: y as usize * stride + x as usize,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layout's own rules, apart from the formula: a block is 64 bytes by 16 rows, 1 KiB,
    /// its first GOB the top 8 rows; inside a GOB a 16-byte row segment is contiguous, two rows
    /// interleave per 16 bytes, and 32-byte halves follow the row pairs.
    #[test]
    fn block_linear_follows_the_16bx2_gob() {
        let pitch = 256;
        assert_eq!(block_linear(0, 0, pitch), 0);
        assert_eq!(block_linear(15, 0, pitch), 15);
        assert_eq!(block_linear(0, 1, pitch), 16);
        assert_eq!(block_linear(16, 0, pitch), 32);
        assert_eq!(block_linear(0, 2, pitch), 64);
        assert_eq!(block_linear(32, 0, pitch), 256);
        assert_eq!(block_linear(0, 8, pitch), 512);
        assert_eq!(block_linear(64, 0, pitch), 1024);
        assert_eq!(block_linear(0, 16, pitch), 16 * pitch);
        assert_eq!(block_linear(255, 15, pitch), 4 * 1024 - 1);
    }

    #[test]
    fn block_linear_is_a_permutation_of_each_block_row() {
        let (pitch, rows) = (192, 32);
        let mut seen = vec![false; (pitch * rows) as usize];
        for y in 0..rows {
            for x in 0..pitch {
                let a = block_linear(x, y, pitch) as usize;
                assert!(!seen[a], "{x},{y} lands on a taken byte");
                seen[a] = true;
            }
        }
        assert!(seen.iter().all(|&s| s));
    }

    #[test]
    fn check_atoms_stay_inside_both_layouts() {
        let s = TiledSurface {
            base: std::ptr::null(),
            size: 0,
            pitch: 1920,
            chroma_offset: 1920 * 1088,
            width: 1920,
            height: 1080,
            pool: 0,
        };
        let atoms = atoms(&s, 2048, 2048);
        assert_eq!(atoms.len(), 2 * 16 * 16);
        for a in &atoms {
            assert_eq!(a.surface % 16, 0, "an atom starts a 16-byte segment");
            if a.luma {
                assert!(a.surface + 16 <= u64::from(s.chroma_offset));
                assert!(a.linear + 16 <= 2048 * 1079 + 1920);
            } else {
                assert!(a.surface + 16 <= u64::from(s.chroma_offset) + 1920 * 544);
                assert!(a.linear + 16 <= 2048 * 539 + 1920);
            }
        }
        let rows: std::collections::BTreeSet<u64> = atoms.iter().map(|a| a.linear as u64 / 2048).collect();
        assert!(rows.contains(&0) && rows.contains(&1079), "edge rows are sampled");
    }
}
