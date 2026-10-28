//! A decoded picture on its way from a decode lane to the presenter, the slot between them, and
//! the NV12 upload that turns a CPU picture into one.
//!
//! The slot holds one picture, newest wins: a lane that outruns the display overwrites, and the
//! presenter takes whatever is newest at its vblank. A picture keeps its planes alive through
//! `hold` until the last frame that sampled it has finished on the GPU.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{ensure, Context, Result};
use ash::vk;
use pf_client_video::video_color::ColorDesc;
use pf_nx_app::platform::StreamSpec;

use crate::device::GpuDevice;

/// Plane views in the layout the lane leaves them (`layout`).
#[derive(Clone, Copy, Debug)]
pub enum Planes {
    /// Y, Cb, Cr: `PyroWave`.
    Three([vk::ImageView; 3]),
    /// Y and interleaved `CbCr`: the NV12 upload.
    Two([vk::ImageView; 2]),
}

pub struct Picture {
    pub planes: Planes,
    pub layout: vk::ImageLayout,
    pub width: u32,
    pub height: u32,
    pub color: ColorDesc,
    /// 8 or 10.
    pub depth: u8,
    /// 10-bit codes in the MSBs of 16-bit samples.
    pub msb_packed: bool,
    /// Visible fraction of the plane images (a picture smaller than its pool); `[1.0, 1.0]` when exact.
    pub uv_scale: [f32; 2],
    pub decoded_at: Instant,
    /// Host capture PTS and the realtime decode stamp, for the overlay's display stage.
    pub pts_ns: u64,
    pub decoded_ns: u64,
    /// The present turn the lane offered (`QueueLock::offer_present_turn`), if any.
    pub turn: Option<u64>,
    /// The session's overlay figures, where the display stage is noted.
    pub hud: Option<Arc<punktfunk_core::hud::Stats>>,
    /// Keeps the planes' images and memory alive.
    pub hold: Box<dyn std::any::Any + Send + Sync>,
}

/// One picture between a lane and the presenter.
#[derive(Default)]
pub struct VideoSlot {
    newest: Mutex<Option<Arc<Picture>>>,
}

impl VideoSlot {
    /// Replaces whatever the presenter has not taken yet.
    pub fn publish(&self, picture: Picture) {
        *self.newest.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(picture));
    }

    /// The newest picture, if one arrived since the last take.
    pub fn take(&self) -> Option<Arc<Picture>> {
        self.newest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// Drops a waiting picture: the stream ended.
    pub fn clear(&self) {
        self.take();
    }
}

/// Rows in a staging buffer start this many bytes apart, at least.
const ROW_ALIGN: usize = 256;
/// Staging buffers: the decoder fills one while the GPU copies out of the other.
const COPIES: usize = 2;
/// Device-local plane sets: one per frame in flight, one waiting in the slot, one being written.
const PLANE_SETS: usize = crate::ui::FRAMES + 2;

/// Where the decoder writes the next picture: NV12, `y_stride` and `uv_stride` bytes a row.
pub struct StagingPlanes<'a> {
    pub y: &'a mut [u8],
    pub y_stride: usize,
    pub uv: &'a mut [u8],
    pub uv_stride: usize,
}

/// The device-local planes one picture is copied into.
struct PlaneSet {
    dev: Arc<GpuDevice>,
    images: [vk::Image; 2],
    memory: [vk::DeviceMemory; 2],
    views: [vk::ImageView; 2],
}

impl Drop for PlaneSet {
    fn drop(&mut self) {
        // SAFETY: the last holder drops this after its sampling frame finished, and the upload
        // that writes it idles the device before letting go of it. Null views are no-ops.
        unsafe {
            for i in 0..2 {
                self.dev.device.destroy_image_view(self.views[i], None);
                self.dev.device.destroy_image(self.images[i], None);
                self.dev.device.free_memory(self.memory[i], None);
            }
        }
    }
}

/// One staging buffer, mapped for its life, and the copy out of it.
struct Staging {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    ptr: *mut u8,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    in_flight: bool,
}

/// NV12 pictures from the CPU into device-local planes, published to the presenter's slot.
///
/// [`Self::stage`] hands out the next staging buffer, [`Self::commit`] records its copy into a
/// free plane set and publishes the picture. The copy is not waited: the presenter samples on
/// the same queue after it, and a staging buffer is waited only when it comes round again.
/// Every plane set still on screen skips the picture: a dropped frame, never a reason to ask
/// for a keyframe.
pub struct Nv12Upload {
    dev: Arc<GpuDevice>,
    slot: Arc<VideoSlot>,
    /// The plane images' luma extent; pictures up to this size fit.
    extent: vk::Extent2D,
    stride: usize,
    color: ColorDesc,
    hud: Arc<punktfunk_core::hud::Stats>,
    planes: Vec<Arc<PlaneSet>>,
    pool: vk::CommandPool,
    staging: Vec<Staging>,
    next: usize,
}

// SAFETY: the mapped pointers address memory this upload owns for its whole life and only
// `&mut self` writes through them; the command pool is used by whichever single thread holds it.
unsafe impl Send for Nv12Upload {}

impl Nv12Upload {
    /// Staging and planes for `spec`'s picture size, publishing to `slot`.
    pub fn new(dev: Arc<GpuDevice>, slot: Arc<VideoSlot>, spec: &StreamSpec) -> Result<Self> {
        let extent = vk::Extent2D {
            width: spec.width.next_multiple_of(2),
            height: spec.height.next_multiple_of(2),
        };
        let stride = (extent.width as usize).next_multiple_of(ROW_ALIGN);
        let bytes = stride * extent.height as usize * 3 / 2;
        // SAFETY: builders are locals; the pool is owned here and destroyed in `Drop`.
        let pool = unsafe {
            dev.device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(dev.qf)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )
        }?;
        let mut upload = Self {
            dev,
            slot,
            extent,
            stride,
            color: ColorDesc {
                primaries: spec.color.primaries,
                transfer: spec.color.transfer,
                matrix: spec.color.matrix,
                full_range: spec.color.full_range != 0,
            },
            hud: spec.hud.clone(),
            planes: Vec::new(),
            pool,
            staging: Vec::with_capacity(COPIES),
            next: 0,
        };
        let dev = &upload.dev;
        // SAFETY: the pool is live; the buffers are freed with it.
        let cmds = unsafe {
            dev.device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(COPIES as u32),
            )
        }?;
        for cmd in cmds {
            let (buffer, memory, ptr) =
                crate::util::host_buffer(dev, bytes as u64, vk::BufferUsageFlags::TRANSFER_SRC)?;
            // SAFETY: the create-info is a local; the fence is destroyed in `Drop`.
            let fence = unsafe { dev.device.create_fence(&vk::FenceCreateInfo::default(), None) };
            upload.staging.push(Staging {
                buffer,
                memory,
                ptr,
                cmd,
                fence: fence.inspect_err(|_| {
                    // SAFETY: the buffer was never submitted.
                    unsafe {
                        dev.device.destroy_buffer(buffer, None);
                        dev.device.free_memory(memory, None);
                    }
                })?,
                in_flight: false,
            });
        }
        tracing::info!(
            "upload: NV12 {}x{} through {COPIES} staging buffers of {bytes} bytes, row {stride}",
            extent.width,
            extent.height
        );
        Ok(upload)
    }

    /// The staging buffer the next picture is written into. Waits out that buffer's last copy.
    pub fn stage(&mut self) -> Result<StagingPlanes<'_>> {
        let i = self.next;
        self.wait(i)?;
        let y_len = self.stride * self.extent.height as usize;
        let len = y_len * 3 / 2;
        // SAFETY: `ptr` maps `len` bytes of host-coherent memory this upload owns; the copy that
        // last read it has finished (waited above), and the borrow of `self` keeps every other
        // access out until the planes are dropped.
        let all = unsafe { std::slice::from_raw_parts_mut(self.staging[i].ptr, len) };
        let (y, uv) = all.split_at_mut(y_len);
        Ok(StagingPlanes {
            y,
            y_stride: self.stride,
            uv,
            uv_stride: self.stride,
        })
    }

    /// Copies the staged `width`×`height` picture into a free plane set and publishes it.
    /// `false` when every set is still on screen and the picture was dropped.
    pub fn commit(&mut self, width: u32, height: u32, pts_ns: u64) -> Result<bool> {
        ensure!(
            width <= self.extent.width && height <= self.extent.height,
            "a {width}x{height} picture outgrows the {}x{} upload",
            self.extent.width,
            self.extent.height
        );
        let Some(planes) = self.free_planes()? else {
            tracing::warn!("upload: every plane set is still on screen — dropping this picture");
            return Ok(false);
        };
        let i = self.next;
        self.next = (i + 1) % COPIES;
        let s = &self.staging[i];
        let dev = &self.dev;
        let layers = vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            layer_count: 1,
            ..Default::default()
        };
        let y = [vk::BufferImageCopy::default()
            .buffer_row_length(self.stride as u32)
            .image_subresource(layers)
            .image_extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })];
        let uv = [vk::BufferImageCopy::default()
            .buffer_offset((self.stride * self.extent.height as usize) as u64)
            .buffer_row_length(self.stride as u32 / 2)
            .image_subresource(layers)
            .image_extent(vk::Extent3D {
                width: width.div_ceil(2),
                height: height.div_ceil(2),
                depth: 1,
            })];
        // SAFETY: the command buffer and fence are this upload's and idle (`stage` waited them);
        // the images belong to the plane set, which only this upload writes while it is free.
        unsafe {
            dev.device
                .reset_command_buffer(s.cmd, vk::CommandBufferResetFlags::empty())?;
            dev.device.begin_command_buffer(
                s.cmd,
                &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            for image in planes.images {
                // After the presenter's last read and any earlier copy into this set, which a
                // replaced picture can leave in flight.
                crate::util::barrier(
                    dev,
                    s.cmd,
                    image,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    (
                        vk::PipelineStageFlags2::FRAGMENT_SHADER | vk::PipelineStageFlags2::COPY,
                        vk::AccessFlags2::TRANSFER_WRITE,
                    ),
                    (vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_WRITE),
                );
            }
            dev.device.cmd_copy_buffer_to_image(
                s.cmd,
                s.buffer,
                planes.images[0],
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &y,
            );
            dev.device.cmd_copy_buffer_to_image(
                s.cmd,
                s.buffer,
                planes.images[1],
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &uv,
            );
            for image in planes.images {
                crate::util::barrier(
                    dev,
                    s.cmd,
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    (vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_WRITE),
                    (vk::PipelineStageFlags2::FRAGMENT_SHADER, vk::AccessFlags2::SHADER_READ),
                );
            }
            dev.device.end_command_buffer(s.cmd)?;
            let cmds = [s.cmd];
            dev.vkd.queue_lock.yield_to_present();
            let _q = dev.vkd.queue_lock.guard();
            dev.device
                .queue_submit(dev.queue, &[vk::SubmitInfo::default().command_buffers(&cmds)], s.fence)?;
        }
        self.staging[i].in_flight = true;
        self.slot.publish(Picture {
            planes: Planes::Two(planes.views),
            layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            width,
            height,
            color: self.color,
            depth: 8,
            msb_packed: false,
            uv_scale: [
                width as f32 / self.extent.width as f32,
                height as f32 / self.extent.height as f32,
            ],
            decoded_at: Instant::now(),
            pts_ns,
            decoded_ns: u64::try_from(punktfunk_core::client::now_realtime_ns()).unwrap_or(0),
            turn: None,
            hud: Some(self.hud.clone()),
            hold: Box::new(planes),
        });
        Ok(true)
    }

    /// Waits for staging buffer `i`'s copy to finish, if it is in flight.
    fn wait(&mut self, i: usize) -> Result<()> {
        let s = &mut self.staging[i];
        if std::mem::take(&mut s.in_flight) {
            // SAFETY: the fence is this upload's and was submitted with buffer `i`'s copy.
            unsafe {
                self.dev
                    .device
                    .wait_for_fences(&[s.fence], true, 1_000_000_000)
                    .context("staging copy")?;
                self.dev.device.reset_fences(&[s.fence])?;
            }
        }
        Ok(())
    }

    /// A plane set nothing else holds, or a new one while there are fewer than [`PLANE_SETS`].
    fn free_planes(&mut self) -> Result<Option<Arc<PlaneSet>>> {
        if let Some(p) = self.planes.iter().find(|p| Arc::strong_count(p) == 1) {
            return Ok(Some(p.clone()));
        }
        if self.planes.len() >= PLANE_SETS {
            return Ok(None);
        }
        let usage = vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST;
        let local = vk::MemoryPropertyFlags::DEVICE_LOCAL;
        let chroma = vk::Extent2D {
            width: self.extent.width / 2,
            height: self.extent.height / 2,
        };
        let (yi, ym) = crate::util::image(&self.dev, self.extent, vk::Format::R8_UNORM, usage, local)?;
        let (ci, cm) =
            crate::util::image(&self.dev, chroma, vk::Format::R8G8_UNORM, usage, local).inspect_err(|_| {
                // SAFETY: the luma image was never used.
                unsafe {
                    self.dev.device.destroy_image(yi, None);
                    self.dev.device.free_memory(ym, None);
                }
            })?;
        // Views are filled in place: a failed one drops the set, which destroys what was made.
        let mut set = PlaneSet {
            dev: self.dev.clone(),
            images: [yi, ci],
            memory: [ym, cm],
            views: [vk::ImageView::null(); 2],
        };
        set.views[0] = crate::util::view(&self.dev, yi, vk::Format::R8_UNORM)?;
        set.views[1] = crate::util::view(&self.dev, ci, vk::Format::R8G8_UNORM)?;
        let set = Arc::new(set);
        self.planes.push(set.clone());
        Ok(Some(set))
    }
}

impl Drop for Nv12Upload {
    fn drop(&mut self) {
        {
            let _q = self.dev.vkd.queue_lock.guard();
            // SAFETY: idling the shared device under the queue lock before the buffers go.
            let _ = unsafe { self.dev.device.device_wait_idle() };
        }
        // SAFETY: the device is idle and every handle below is this upload's.
        unsafe {
            for s in self.staging.drain(..) {
                self.dev.device.destroy_fence(s.fence, None);
                self.dev.device.destroy_buffer(s.buffer, None);
                self.dev.device.free_memory(s.memory, None);
            }
            self.dev.device.destroy_command_pool(self.pool, None);
        }
    }
}
