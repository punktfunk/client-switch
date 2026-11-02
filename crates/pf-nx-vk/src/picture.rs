//! A decoded picture on its way from a decode lane to the presenter, the slot between them, and
//! the NV12 upload that turns a CPU picture into one.
//!
//! Under the latency intent the slot holds one picture, newest wins: a lane that outruns the
//! display overwrites, and the presenter takes whatever is newest at its vblank. Under smoothness
//! it is a short queue: each picture waits for its due time and they go out in order, so two that
//! land in one refresh are shown on two instead of one replacing the other. A picture keeps its
//! planes alive through `hold` until the last frame that sampled it has finished on the GPU.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{ensure, Context, Result};
use ash::vk;
use pf_client_video::video_color::ColorDesc;
use pf_nx_app::platform::StreamSpec;

use crate::device::GpuDevice;
use crate::timer::GpuTimer;

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
    /// When the session's pacing wants it on glass. Only a smoothing slot reads it; `None` is due
    /// at once.
    pub due: Option<Instant>,
    /// Host capture PTS and the realtime decode stamp, for the overlay's display stage.
    pub pts_ns: u64,
    pub decoded_ns: u64,
    /// The present turn the lane offered (`QueueLock::offer_present_turn`), if any.
    pub turn: Option<u64>,
    /// The session's overlay figures, where the display stage is noted.
    pub hud: Option<Arc<punktfunk_core::hud::Stats>>,
    /// Where the presenter leaves the capture-to-glass time for the audio's A/V sync.
    pub e2e: Option<pf_nx_app::platform::VideoE2e>,
    /// Keeps the planes' images and memory alive.
    pub hold: Box<dyn std::any::Any + Send + Sync>,
}

/// The pictures between a lane and the presenter.
#[derive(Default)]
pub struct VideoSlot {
    queue: Mutex<Queue<Arc<Picture>>>,
}

impl VideoSlot {
    /// `0` is the latency intent, newest wins; `1..=3` is the smoothness budget in frames.
    pub fn set_smoothing(&self, frames: usize) {
        self.lock().set_smoothing(frames);
    }

    /// Queues `picture`; under the latency intent it replaces whatever was not taken yet.
    pub fn publish(&self, picture: Picture) {
        let due = picture.due;
        self.lock().push(due, Arc::new(picture));
    }

    /// The picture for the vblank at `latch`, if a new one is due by then.
    pub fn take(&self, latch: Instant) -> Option<Arc<Picture>> {
        let mut q = self.lock();
        let picture = q.take(latch);
        if q.smoothing > 0 {
            if let Some(w) = q.window.close(latch) {
                tracing::info!("{}", w.line());
            }
        }
        picture
    }

    /// Drops the waiting pictures: the stream ended.
    pub fn clear(&self) {
        self.lock().clear();
    }

    /// Pictures dropped unseen since the last call — see [`Drops`].
    pub fn take_drops(&self) -> Drops {
        std::mem::take(&mut self.lock().drops)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Queue<Arc<Picture>>> {
        self.queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The largest smoothness budget, `PresentPriority`'s clamp.
const MAX_SMOOTHING: usize = 3;
/// Pictures a smoothing queue holds past its budget: the pacing's own cushion, up to a period,
/// and one picture of slack before the oldest is dropped.
const SMOOTHING_SLACK: usize = 2;
/// The most pictures the slot ever holds.
const MAX_QUEUED: usize = MAX_SMOOTHING + SMOOTHING_SLACK;

/// Pictures the slot dropped without showing them, the figure the smoothness intent exists to
/// keep at zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Drops {
    /// Passed over for a newer one: newest wins, or a smoothing backlog caught up.
    pub replaced: u32,
    /// Pushed out of a full smoothing queue.
    pub overflow: u32,
}

/// How often the slot reports how its pictures met their due times.
const WINDOW: std::time::Duration = std::time::Duration::from_secs(10);

/// One window of the smoothing slot's takes: whether a due time ever held a picture back, how
/// late the ones taken were against it, and how many waited behind each. A standing picture
/// behind every take is a refresh of latency the budget did not ask for.
#[derive(Default)]
struct Window {
    since: Option<Instant>,
    taken: u32,
    /// Vblanks with a picture waiting that was not due yet.
    held: u32,
    /// Taken with no due time.
    undue: u32,
    /// Taken before their due time, to keep the queue inside the budget.
    early: u32,
    /// Latch less due over the pictures taken at or past it, µs.
    late_us: u64,
    late_max_us: u64,
    late_n: u32,
    /// Pictures left waiting after each take: none, one, two or more.
    left: [u32; 3],
}

impl Window {
    fn note_take(&mut self, latch: Instant, due: Option<Instant>, left: usize) {
        self.taken += 1;
        self.left[left.min(2)] += 1;
        match due {
            None => self.undue += 1,
            Some(d) if d > latch => self.early += 1,
            Some(d) => {
                let us = u64::try_from((latch - d).as_micros()).unwrap_or(u64::MAX);
                self.late_us += us;
                self.late_max_us = self.late_max_us.max(us);
                self.late_n += 1;
            }
        }
    }

    /// The finished window at `now`, arming the next; `None` until one has run its length.
    fn close(&mut self, now: Instant) -> Option<Self> {
        let since = *self.since.get_or_insert(now);
        (now.duration_since(since) >= WINDOW).then(|| {
            std::mem::replace(
                self,
                Self {
                    since: Some(now),
                    ..Self::default()
                },
            )
        })
    }

    fn line(&self) -> String {
        let ms = |us: u64| us as f64 / 1000.0;
        format!(
            "slot[{}s]: {} taken, {} vblanks held for a due, {} without a due, {} early; late mean {:.1}ms max {:.1}ms; left behind 0:{} 1:{} 2+:{}",
            WINDOW.as_secs(),
            self.taken,
            self.held,
            self.undue,
            self.early,
            ms(self.late_us.checked_div(u64::from(self.late_n)).unwrap_or(0)),
            ms(self.late_max_us),
            self.left[0],
            self.left[1],
            self.left[2],
        )
    }
}

/// The slot's rule, apart from the pictures, so the tests drive it with plain values.
struct Queue<T> {
    items: VecDeque<(Option<Instant>, T)>,
    /// `0`: newest wins.
    smoothing: usize,
    drops: Drops,
    /// Smoothing only: the takes since the last report.
    window: Window,
}

impl<T> Default for Queue<T> {
    fn default() -> Self {
        Self {
            items: VecDeque::with_capacity(MAX_QUEUED + 1),
            smoothing: 0,
            drops: Drops::default(),
            window: Window::default(),
        }
    }
}

impl<T> Queue<T> {
    fn set_smoothing(&mut self, frames: usize) {
        self.smoothing = frames.min(MAX_SMOOTHING);
        self.trim();
    }

    fn push(&mut self, due: Option<Instant>, item: T) {
        if self.smoothing == 0 {
            self.drops.replaced += self.items.len() as u32;
            self.items.clear();
        }
        self.items.push_back((due, item));
        self.trim();
    }

    /// Newest wins ignores `due`. Smoothing shows the oldest picture due by `latch` and lets one
    /// more due wait for the next vblank: due times that straddle a vblank then take one refresh
    /// each. A third due is a backlog, and its oldest are dropped so latency stays the budget.
    /// More waiting than the budget holds means the due times run ahead of the arrivals, and the
    /// oldest goes out early.
    fn take(&mut self, latch: Instant) -> Option<T> {
        if self.smoothing == 0 {
            return self.items.pop_back().map(|(_, item)| item);
        }
        let due = self
            .items
            .iter()
            .take_while(|(due, _)| due.is_none_or(|d| d <= latch))
            .count();
        // After a network stall the pacing can re-anchor late, putting every due time past what
        // the queue holds: waiting for them trimmed each picture unseen while the screen stood
        // still for seconds (smoothing log of 2026-10-31, 151 trimmed in one window).
        let due = due.max(self.items.len().saturating_sub(self.smoothing + 1));
        if due == 0 {
            self.window.held += u32::from(!self.items.is_empty());
            return None;
        }
        let behind = due.saturating_sub(2);
        self.drops.replaced += behind as u32;
        self.items.drain(..behind);
        let (at, item) = self.items.pop_front()?;
        self.window.note_take(latch, at, self.items.len());
        Some(item)
    }

    fn clear(&mut self) {
        self.items.clear();
    }

    fn trim(&mut self) {
        let cap = if self.smoothing == 0 {
            1
        } else {
            self.smoothing + SMOOTHING_SLACK
        };
        while self.items.len() > cap {
            self.items.pop_front();
            self.drops.overflow += 1;
        }
    }
}

/// Rows in a staging buffer start this many bytes apart, at least.
const ROW_ALIGN: usize = 256;
/// Staging buffers: the decoder fills one while the GPU copies out of the other.
const COPIES: usize = 2;
/// Device-local plane sets: one per frame in flight, the most the slot queues, one being written.
/// Made as needed, so the latency intent never makes more than it did with one waiting.
const PLANE_SETS: usize = crate::ui::FRAMES + MAX_QUEUED + 1;

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
    e2e: pf_nx_app::platform::VideoE2e,
    planes: Vec<Arc<PlaneSet>>,
    pool: vk::CommandPool,
    staging: Vec<Staging>,
    next: usize,
    /// Each staging buffer's copy on the GPU; `None` where the queue has no timestamps.
    timer: Option<GpuTimer>,
    /// The last copy's GPU time that [`Self::stage`] read back.
    copy_gpu_us: Option<u32>,
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
            e2e: spec.video_e2e.clone(),
            planes: Vec::new(),
            pool,
            staging: Vec::with_capacity(COPIES),
            next: 0,
            timer: None,
            copy_gpu_us: None,
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
        upload.timer = GpuTimer::new(&upload.dev, COPIES);
        tracing::info!(
            "upload: NV12 {}x{} through {COPIES} staging buffers of {bytes} bytes, row {stride}, at {:p}",
            extent.width,
            extent.height,
            upload.staging[0].ptr,
        );
        upload.slot.set_smoothing(spec.smoothing);
        if spec.smoothing == 0 {
            tracing::info!("presenter: newest picture wins at each vblank");
        } else {
            tracing::info!(
                "presenter: pictures in order at their due time, smoothing {} frames",
                spec.smoothing.min(MAX_SMOOTHING)
            );
        }
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

    /// Copies the staged `width`×`height` picture into a free plane set and publishes it, due on
    /// glass at `due`. `false` when every set is still on screen and the picture was dropped.
    pub fn commit(&mut self, width: u32, height: u32, pts_ns: u64, due: Option<Instant>) -> Result<bool> {
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
            if let Some(t) = &mut self.timer {
                t.begin(dev, s.cmd, i);
            }
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
            if let Some(t) = &self.timer {
                t.end(dev, s.cmd, i);
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
            due,
            pts_ns,
            decoded_ns: u64::try_from(punktfunk_core::client::now_realtime_ns()).unwrap_or(0),
            turn: None,
            hud: Some(self.hud.clone()),
            e2e: Some(self.e2e.clone()),
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
            self.copy_gpu_us = self.timer.as_ref().and_then(|t| t.read(&self.dev, i));
        }
        Ok(())
    }

    /// The GPU time of the last staging copy that has finished, where the GPU can time it.
    pub fn copy_gpu_us(&self) -> Option<u32> {
        self.copy_gpu_us
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
            if let Some(t) = &self.timer {
                t.destroy(&self.dev);
            }
            for s in self.staging.drain(..) {
                self.dev.device.destroy_fence(s.fence, None);
                self.dev.device.destroy_buffer(s.buffer, None);
                self.dev.device.free_memory(s.memory, None);
            }
            self.dev.device.destroy_command_pool(self.pool, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn ms(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    #[test]
    fn newest_wins_under_the_latency_intent() {
        let t0 = Instant::now();
        let mut q = Queue::default();
        q.push(Some(ms(t0, 500)), 1);
        q.push(Some(ms(t0, 500)), 2);
        assert_eq!(q.take(t0), Some(2), "the due time is not read");
        assert_eq!(q.take(t0), None);
        assert_eq!((q.drops.replaced, q.drops.overflow), (1, 0));
    }

    #[test]
    fn two_in_one_refresh_take_one_each() {
        let t0 = Instant::now();
        let mut q = Queue::default();
        q.set_smoothing(1);
        q.push(Some(ms(t0, 10)), 1);
        q.push(Some(ms(t0, 15)), 2);
        assert_eq!(q.take(ms(t0, 16)), Some(1));
        assert_eq!(q.take(ms(t0, 33)), Some(2), "the second waits for the next vblank");
        assert_eq!(q.take(ms(t0, 50)), None);
        assert_eq!(q.drops, Drops::default(), "nothing was lost");
    }

    #[test]
    fn a_picture_waits_for_its_due_time() {
        let t0 = Instant::now();
        let mut q = Queue::default();
        q.set_smoothing(2);
        q.push(Some(ms(t0, 40)), 1);
        assert_eq!(q.take(ms(t0, 16)), None);
        assert_eq!(q.take(ms(t0, 33)), None);
        assert_eq!(q.take(ms(t0, 50)), Some(1));
    }

    #[test]
    fn a_backlog_drops_its_oldest_to_keep_the_budget() {
        let t0 = Instant::now();
        let mut q = Queue::default();
        q.set_smoothing(1);
        for i in 0..3 {
            q.push(Some(ms(t0, i * 16)), i);
        }
        assert_eq!(q.take(ms(t0, 100)), Some(1), "three due: the oldest goes, one waits");
        assert_eq!(q.take(ms(t0, 116)), Some(2));
        assert_eq!((q.drops.replaced, q.drops.overflow), (1, 0));
    }

    #[test]
    fn the_queue_is_bounded_past_its_budget() {
        let t0 = Instant::now();
        let mut q = Queue::default();
        q.set_smoothing(1);
        for i in 0..10 {
            q.push(Some(ms(t0, 1_000)), i);
        }
        assert_eq!(q.items.len(), 1 + SMOOTHING_SLACK);
        assert_eq!(q.items.front().map(|(_, i)| *i), Some(7));
        assert_eq!((q.drops.replaced, q.drops.overflow), (0, 7));
    }

    #[test]
    fn a_queue_ahead_of_its_due_times_still_moves() {
        let t0 = Instant::now();
        let mut q = Queue::default();
        q.set_smoothing(1);
        q.push(Some(ms(t0, 1_000)), 0);
        q.push(Some(ms(t0, 1_016)), 1);
        assert_eq!(q.take(t0), None, "two within the budget wait");
        q.push(Some(ms(t0, 1_033)), 2);
        assert_eq!(q.take(ms(t0, 16)), Some(0), "a third goes out early, not trimmed");
        q.push(Some(ms(t0, 1_050)), 3);
        assert_eq!(q.take(ms(t0, 33)), Some(1));
        assert_eq!(q.drops, Drops::default());
    }

    #[test]
    fn no_due_time_is_due_at_once() {
        let t0 = Instant::now();
        let mut q = Queue::default();
        q.set_smoothing(3);
        q.push(None, 1);
        assert_eq!(q.take(t0), Some(1));
    }

    /// The window says whether a due time held anything, how late the takes ran and what
    /// stood behind each: the figures that tell a binding budget from a plain queue.
    #[test]
    fn the_window_counts_holds_lateness_and_what_waits() {
        let t0 = Instant::now();
        let mut q = Queue::default();
        q.set_smoothing(1);
        q.push(Some(ms(t0, 40)), 1);
        assert_eq!(q.take(ms(t0, 16)), None, "not due: held");
        q.push(Some(ms(t0, 45)), 2);
        assert_eq!(q.take(ms(t0, 50)), Some(1), "10 ms late, one behind it");
        q.push(None, 3);
        assert_eq!(q.take(ms(t0, 66)), Some(2));
        assert_eq!(q.take(ms(t0, 83)), Some(3));
        let w = &q.window;
        assert_eq!((w.taken, w.held, w.undue, w.early), (3, 1, 1, 0));
        assert_eq!((w.late_n, w.late_max_us), (2, 21_000));
        assert_eq!(w.left, [1, 2, 0]);
        assert!(q.window.close(ms(t0, 0)).is_none(), "the first close arms the window");
        assert!(q.window.close(ms(t0, 10_000)).is_some());
        assert_eq!(q.window.taken, 0, "the next window starts empty");
    }
}
