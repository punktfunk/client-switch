//! The presenter: the [`Display`] seam on the one device. Each frame clears to black, draws the
//! newest stream picture into its fitted rectangle (when the stream layer is on), blends the UI
//! layer over it (unless a stream switched that layer off), and presents at the next vblank, one
//! frame queued at most where the display has a vsync event ([`crate::pacing`]).
//!
//! Pictures arrive from the decode lanes through a [`VideoSlot`]: newest wins, and the last one
//! shown is drawn again until a newer one arrives. A picture stays alive until the frame that
//! sampled it has finished on the GPU. A swapchain the window outgrew (a dock change, or the
//! driver's out-of-date) is rebuilt before the next frame, UI layer included.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use ash::vk;
use pf_client_video::video_color::csc_rows;
use pf_nx_app::platform::{Display, Fit, FrameReport, Refresh, Rgba};

use crate::device::{GpuDevice, Target};
use crate::output::Output;
use crate::pacing::{Pace, Pacing};
use crate::picture::{Picture, Planes, VideoSlot};
use crate::pipelines::Pipelines;
use crate::timer::GpuTimer;
use crate::ui::{UiLayer, FRAMES};

/// Tonemap source peak for a PQ stream on this SDR output, in 203-nit units (~1000 nits).
const TONEMAP_PEAK: f32 = 4.9;

struct FrameRes {
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    /// Signalled by the acquire; waited by this frame's submit.
    ready: vk::Semaphore,
    /// planar, two-plane, overlay, tiled.
    sets: [vk::DescriptorSet; 4],
    /// The picture this frame sampled, released once its fence signals.
    in_use: Option<Arc<Picture>>,
}

/// Read-back: an image the composition is drawn into again, and a buffer it is copied to.
struct Readback {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    framebuffer: vk::Framebuffer,
    buffer: vk::Buffer,
    buffer_memory: vk::DeviceMemory,
    ptr: *mut u8,
}

pub struct Presenter {
    dev: Arc<GpuDevice>,
    output: Output,
    pipes: Pipelines,
    framebuffers: Vec<vk::Framebuffer>,
    /// Per output image: signalled when its frame is drawn, waited by the present.
    done: Vec<vk::Semaphore>,
    pool: vk::CommandPool,
    desc_pool: vk::DescriptorPool,
    frames: Vec<FrameRes>,
    frame: usize,
    ui: UiLayer,
    /// The UI layer is drawn and blended; a stream with an empty overlay turns it off.
    ui_on: bool,
    slot: Arc<VideoSlot>,
    current: Option<Arc<Picture>>,
    stream: Option<Fit>,
    last_present: Option<Instant>,
    readback: Option<Readback>,
    readback_sets: [vk::DescriptorSet; 4],
    /// The extent asked of a surface that leaves it to the caller.
    want: vk::Extent2D,
    /// The swapchain no longer matches the window: rebuild before the next acquire.
    stale: bool,
    /// Each frame's command buffer on the GPU; `None` where the queue has no timestamps.
    timer: Option<GpuTimer>,
    pace: Pace,
}

// SAFETY: the read-back pointer maps memory this struct owns; the presenter lives on the main
// thread and is moved there once, never shared.
unsafe impl Send for Presenter {}

impl Presenter {
    /// Builds the output, pipelines and UI layer on `dev`. `slot` is shared with the lanes. A vi
    /// target's vsync event, where it has one, paces the loop ([`Self::set_pacing`]).
    pub fn new(dev: Arc<GpuDevice>, slot: Arc<VideoSlot>) -> Result<Self> {
        let want = match dev.target {
            Target::Vi { size, .. } => vk::Extent2D {
                width: size.0,
                height: size.1,
            },
            Target::Offscreen { width, height } => vk::Extent2D { width, height },
        };
        let output = Output::new(&dev, want)?;
        let pipes = Pipelines::new(&dev, output.format, output.final_layout)?;
        let framebuffers = output
            .views
            .iter()
            .map(|&view| framebuffer(&dev, pipes.pass_out, view, output.extent))
            .collect::<Result<Vec<_>>>()?;
        let done = (0..output.images.len())
            .map(|_| semaphore(&dev))
            .collect::<Result<Vec<_>>>()?;
        // SAFETY: builders are locals; the pool is owned here and destroyed in `Drop`.
        let pool = unsafe {
            dev.device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(dev.qf)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )
        }?;
        let sets_needed = (FRAMES as u32 + 1) * 4;
        let sizes = [
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(sets_needed * 3),
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::UNIFORM_TEXEL_BUFFER)
                .descriptor_count((FRAMES as u32 + 1) * 2),
        ];
        // SAFETY: as above.
        let desc_pool = unsafe {
            dev.device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .max_sets(sets_needed)
                    .pool_sizes(&sizes),
                None,
            )
        }?;
        // SAFETY: the pool is live; the buffers are freed with it.
        let cmds = unsafe {
            dev.device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(FRAMES as u32),
            )
        }?;
        let mut frames = Vec::with_capacity(FRAMES);
        for cmd in cmds {
            frames.push(FrameRes {
                cmd,
                // SAFETY: signalled, so the first wait passes; destroyed in `Drop`.
                fence: unsafe {
                    dev.device.create_fence(
                        &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                        None,
                    )
                }?,
                ready: semaphore(&dev)?,
                sets: alloc_sets(&dev, desc_pool, &pipes)?,
                in_use: None,
            });
        }
        let readback_sets = alloc_sets(&dev, desc_pool, &pipes)?;
        let ui = UiLayer::new(&dev, output.extent)?;
        let timer = GpuTimer::new(&dev, FRAMES);
        let pace = match dev.target {
            Target::Vi { vsync, queued, .. } => Pace::new(vsync, Some(queued)),
            Target::Offscreen { .. } => Pace::new(None, None),
        };
        if timer.is_none() {
            tracing::info!("presenter: the queue has no timestamps, so no GPU time per frame");
        }
        // The presenter runs on the main thread, decode on the pump: take present turns so a
        // picture handed over is presented ahead of the next decode on the shared queue.
        dev.vkd.queue_lock.take_turns();
        tracing::info!("presenter: {}", dev.feature_line());
        Ok(Self {
            dev,
            output,
            pipes,
            framebuffers,
            done,
            pool,
            desc_pool,
            frames,
            frame: 0,
            ui,
            ui_on: true,
            slot,
            current: None,
            stream: None,
            last_present: None,
            readback: None,
            readback_sets,
            want,
            stale: false,
            timer,
            pace,
        })
    }

    /// How the loop meets the vblank; [`Pacing::Late`] until set.
    pub fn set_pacing(&mut self, pacing: Pacing) {
        self.pace.set(pacing);
    }

    /// Why the platform has no vsync event; the first `presenter: paced …` line carries it.
    pub fn vsync_refused(&mut self, why: String) {
        self.pace.set_refused(why);
    }

    /// Asks for a `size` swapchain (a dock change); a new size rebuilds the output before the
    /// next frame. A surface that fixes its own extent keeps it.
    pub fn resize(&mut self, size: (u32, u32)) {
        let want = vk::Extent2D {
            width: size.0,
            height: size.1,
        };
        if want != self.want {
            self.want = want;
            self.stale = true;
        }
    }

    /// Swaps the output for one matching the window now, and everything drawn against it. A
    /// new extent also rebuilds the UI layer at that size, and the late take forgets its GPU
    /// readings. A failed rebuild leaves the presenter stale, and the next frame tries again.
    // TODO(verify): that NVK's WSI lets a vi surface be destroyed and made again on the same NWindow.
    fn rebuild(&mut self) -> Result<()> {
        self.wait_idle();
        self.stale = true;
        self.pace.reset_gpu();
        let dev = self.dev.clone();
        // SAFETY: the device is idle; every handle below was created by this presenter, and
        // each destroy here is a no-op when a failed rebuild already ran it.
        unsafe {
            if let Some(rb) = self.readback.take() {
                destroy_readback(&dev, &rb);
            }
            for fb in self.framebuffers.drain(..) {
                dev.device.destroy_framebuffer(fb, None);
            }
            self.output.destroy(&dev);
        }
        self.output = Output::new(&dev, self.want)?;
        let pipes = Pipelines::new(&dev, self.output.format, self.output.final_layout)?;
        // SAFETY: the device is idle and the replacement exists.
        unsafe { self.pipes.destroy(&dev) };
        self.pipes = pipes;
        self.framebuffers = self
            .output
            .views
            .iter()
            .map(|&view| framebuffer(&dev, self.pipes.pass_out, view, self.output.extent))
            .collect::<Result<Vec<_>>>()?;
        while self.done.len() < self.output.images.len() {
            self.done.push(semaphore(&dev)?);
        }
        if self.ui.extent() != self.output.extent {
            let ui = UiLayer::new(&dev, self.output.extent)?;
            // SAFETY: the device is idle, so nothing reads the old layer.
            unsafe { self.ui.destroy(&dev) };
            self.ui = ui;
            tracing::info!("presenter: {}", self.ui.describe());
        }
        // SAFETY: the device is idle, so no set from this pool is in use.
        unsafe {
            dev.device
                .reset_descriptor_pool(self.desc_pool, vk::DescriptorPoolResetFlags::empty())
        }?;
        for f in 0..self.frames.len() {
            self.frames[f].sets = alloc_sets(&dev, self.desc_pool, &self.pipes)?;
        }
        self.readback_sets = alloc_sets(&dev, self.desc_pool, &self.pipes)?;
        self.stale = false;
        Ok(())
    }

    pub fn device(&self) -> &Arc<GpuDevice> {
        &self.dev
    }

    /// How the UI layer draws ([`UiLayer::describe`]).
    pub fn ui(&self) -> String {
        self.ui.describe()
    }

    /// The viewport a `w`×`h` picture takes in `extent` under `fit`.
    fn fitted(fit: Fit, w: u32, h: u32, extent: vk::Extent2D) -> vk::Viewport {
        let (ew, eh) = (extent.width as f32, extent.height as f32);
        let (pw, ph) = (w.max(1) as f32, h.max(1) as f32);
        let (vw, vh) = match fit {
            Fit::Stretch => (ew, eh),
            Fit::Contain => {
                let s = (ew / pw).min(eh / ph);
                (pw * s, ph * s)
            }
            Fit::Cover => {
                let s = (ew / pw).max(eh / ph);
                (pw * s, ph * s)
            }
        };
        vk::Viewport {
            x: (ew - vw) / 2.0,
            y: (eh - vh) / 2.0,
            width: vw,
            height: vh,
            min_depth: 0.0,
            max_depth: 1.0,
        }
    }

    /// Records the composition into `fb`: clear, picture, and the UI layer while it is on. A
    /// picture in place is preceded by a barrier: NVDEC wrote it outside Vulkan, and one the size
    /// of the output has its luma read unfiltered, one texel a pixel.
    ///
    /// # Safety
    /// `cmd` is recording, `fb` belongs to `pass`, and `sets` are not read by work in flight.
    unsafe fn compose(
        &self,
        cmd: vk::CommandBuffer,
        pass: vk::RenderPass,
        fb: vk::Framebuffer,
        sets: [vk::DescriptorSet; 4],
    ) {
        let extent = self.output.extent;
        let full = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent,
        };
        let clear = [vk::ClearValue {
            color: vk::ClearColorValue {
                float32: [0.0, 0.0, 0.0, 1.0],
            },
        }];
        let dev = &self.dev.device;
        let picture = self.stream.and(self.current.as_ref());
        // SAFETY: forwarded from the caller; every handle is owned by this presenter or by the
        // picture it holds for the frame.
        unsafe {
            if picture.is_some_and(|p| matches!(p.planes, Planes::Tiled { .. })) {
                crate::util::memory_barrier(
                    &self.dev,
                    cmd,
                    (
                        vk::PipelineStageFlags2::HOST | vk::PipelineStageFlags2::ALL_COMMANDS,
                        vk::AccessFlags2::HOST_WRITE | vk::AccessFlags2::MEMORY_WRITE,
                    ),
                    (vk::PipelineStageFlags2::FRAGMENT_SHADER, vk::AccessFlags2::SHADER_READ),
                );
            }
            dev.cmd_begin_render_pass(
                cmd,
                &vk::RenderPassBeginInfo::default()
                    .render_pass(pass)
                    .framebuffer(fb)
                    .render_area(full)
                    .clear_values(&clear),
                vk::SubpassContents::INLINE,
            );
            dev.cmd_set_scissor(cmd, 0, &[full]);
            if let (Some(fit), Some(picture)) = (self.stream, picture) {
                let (pipe, set, surface) = match &picture.planes {
                    Planes::Three(v) => {
                        self.write_set(sets[0], v, picture.layout);
                        (&self.pipes.planar, sets[0], None)
                    }
                    Planes::Two(v) => {
                        self.write_set(sets[1], v, picture.layout);
                        (&self.pipes.two_plane, sets[1], None)
                    }
                    Planes::Tiled { views, pitch } => {
                        self.write_texel_set(sets[3], views);
                        let exact = (picture.width, picture.height) == (extent.width, extent.height);
                        (
                            &self.pipes.tiled,
                            sets[3],
                            Some([*pitch, picture.width, picture.height, u32::from(exact)]),
                        )
                    }
                };
                let rows = csc_rows(picture.color, picture.depth, picture.msb_packed);
                let mut pc = [0u32; 20];
                for (w, r) in pc.iter_mut().zip(rows.as_flattened()) {
                    *w = r.to_bits();
                }
                // The output is SDR: a PQ picture is tonemapped.
                pc[12] = if picture.color.is_pq() { 1f32 } else { 0f32 }.to_bits();
                pc[13] = TONEMAP_PEAK.to_bits();
                pc[14] = picture.uv_scale[0].to_bits();
                pc[15] = picture.uv_scale[1].to_bits();
                let len = if let Some(surface) = surface {
                    pc[16..].copy_from_slice(&surface);
                    20
                } else {
                    16
                };
                let words = pc.map(u32::to_ne_bytes);
                dev.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipe.pipeline);
                dev.cmd_set_viewport(cmd, 0, &[Self::fitted(fit, picture.width, picture.height, extent)]);
                dev.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::GRAPHICS, pipe.layout, 0, &[set], &[]);
                dev.cmd_push_constants(
                    cmd,
                    pipe.layout,
                    vk::ShaderStageFlags::FRAGMENT,
                    0,
                    &words.as_flattened()[..len * 4],
                );
                dev.cmd_draw(cmd, 3, 1, 0, 0);
            }
            if self.ui_on {
                self.write_set(sets[2], &[self.ui.view()], vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
                dev.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.pipes.overlay.pipeline);
                dev.cmd_set_viewport(
                    cmd,
                    0,
                    &[vk::Viewport {
                        x: 0.0,
                        y: 0.0,
                        width: extent.width as f32,
                        height: extent.height as f32,
                        min_depth: 0.0,
                        max_depth: 1.0,
                    }],
                );
                dev.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.pipes.overlay.layout,
                    0,
                    &[sets[2]],
                    &[],
                );
                dev.cmd_draw(cmd, 3, 1, 0, 0);
            }
            dev.cmd_end_render_pass(cmd);
        }
    }

    fn write_set(&self, set: vk::DescriptorSet, views: &[vk::ImageView], layout: vk::ImageLayout) {
        let infos: Vec<vk::DescriptorImageInfo> = views
            .iter()
            .map(|&v| {
                vk::DescriptorImageInfo::default()
                    .sampler(self.pipes.sampler)
                    .image_view(v)
                    .image_layout(layout)
            })
            .collect();
        let writes: Vec<vk::WriteDescriptorSet> = infos
            .iter()
            .enumerate()
            .map(|(i, info)| {
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(i as u32)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(std::slice::from_ref(info))
            })
            .collect();
        // SAFETY: `writes` borrows `infos`, both live for the call; the set is not in use by
        // work in flight (its frame's fence was waited, or the device idled for read-back).
        unsafe { self.dev.device.update_descriptor_sets(&writes, &[]) };
    }

    /// Points `set`'s two texel-buffer bindings at a tiled picture's plane views.
    fn write_texel_set(&self, set: vk::DescriptorSet, views: &[vk::BufferView; 2]) {
        let writes: Vec<vk::WriteDescriptorSet> = views
            .iter()
            .enumerate()
            .map(|(i, view)| {
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(i as u32)
                    .descriptor_type(vk::DescriptorType::UNIFORM_TEXEL_BUFFER)
                    .texel_buffer_view(std::slice::from_ref(view))
            })
            .collect();
        // SAFETY: `writes` borrows `views`, both live for the call; the set is not in use by
        // work in flight (its frame's fence was waited, or the device idled for read-back).
        unsafe { self.dev.device.update_descriptor_sets(&writes, &[]) };
    }

    fn ensure_readback(&mut self) -> Result<()> {
        if self.readback.is_some() {
            return Ok(());
        }
        let extent = self.output.extent;
        let format = self.output.format;
        let (image, memory) = crate::util::image(
            &self.dev,
            extent,
            format,
            vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )?;
        let view = crate::util::view(&self.dev, image, format)?;
        let framebuffer = framebuffer(&self.dev, self.pipes.pass_read, view, extent)?;
        let bytes = u64::from(extent.width) * u64::from(extent.height) * 4;
        let (buffer, buffer_memory, ptr) =
            crate::util::host_buffer(&self.dev, bytes, vk::BufferUsageFlags::TRANSFER_DST)?;
        self.readback = Some(Readback {
            image,
            memory,
            view,
            framebuffer,
            buffer,
            buffer_memory,
            ptr,
        });
        Ok(())
    }

    fn wait_idle(&self) {
        let _q = self.dev.vkd.queue_lock.guard();
        // SAFETY: idling a live device under the queue lock.
        let _ = unsafe { self.dev.device.device_wait_idle() };
    }

    /// The next output image; a swapchain the driver calls out of date is rebuilt and asked again.
    fn acquire(&mut self, ready: vk::Semaphore) -> Result<u32> {
        if let Some(index) = self.output.acquire(ready)? {
            return Ok(index);
        }
        self.rebuild()?;
        self.output
            .acquire(ready)?
            .ok_or_else(|| anyhow!("the swapchain is out of date right after a rebuild"))
    }
}

impl Display for Presenter {
    fn size(&self) -> (u32, u32) {
        (self.output.extent.width, self.output.extent.height)
    }

    fn ui_size(&self) -> (u32, u32) {
        let e = self.ui.extent();
        (e.width, e.height)
    }

    fn refresh(&self) -> Refresh {
        Refresh(self.output.refresh_mhz)
    }

    fn frame(&mut self, ui: &mut dyn FnMut(&skia_safe::Canvas)) -> Result<FrameReport> {
        let f = self.frame;
        let (cmd, fence, ready) = {
            let fr = &self.frames[f];
            (fr.cmd, fr.fence, fr.ready)
        };
        // SAFETY: the fence belongs to this presenter; waiting only reads.
        unsafe { self.dev.device.wait_for_fences(&[fence], true, u64::MAX) }
            .map_err(|e| anyhow!("wait_for_fences: {e}"))?;
        self.frames[f].in_use = None;
        let gpu_us = self.timer.as_ref().and_then(|t| t.read(&self.dev, f));
        if let Some(us) = gpu_us {
            self.pace.note_gpu(us);
        }
        if self.stale {
            self.rebuild()?;
        }

        if self.ui_on {
            self.ui.draw(ui);
        }
        let period = Duration::from_nanos(self.refresh().period_ns());
        let presents = self.output.presents();
        if presents {
            self.pace.announce();
        }
        // Paced on the vblank, the acquire follows one and finds an image free at once. Paced by
        // the acquire, it blocks until the panel frees one, just after a vblank.
        let vblank = self.pace.wait(period);
        let asked = Instant::now();
        let index = self.acquire(ready)?;
        let quick = !presents
            || self
                .pace
                .acquired(asked.elapsed(), self.last_present.map(|t| asked - t), period);
        let mut report = FrameReport {
            gpu_us,
            ..FrameReport::default()
        };
        if self.stream.is_some() {
            // The frame latches at the vblank after the one waited for. Without one, or after an
            // acquire that blocked, at the vblank a refresh from here.
            let latch = match vblank.filter(|_| quick) {
                Some(at) => {
                    self.pace.take(at, period);
                    at + period
                }
                None => Instant::now() + period,
            };
            if let Some(picture) = self.slot.take(latch) {
                if let Some(turn) = picture.turn {
                    self.dev.vkd.queue_lock.begin_present_turn(turn);
                }
                report.new_picture = true;
                report.decode_to_present_us = u32::try_from(picture.decoded_at.elapsed().as_micros()).ok();
                self.current = Some(picture);
            }
            let drops = self.slot.take_drops();
            report.replaced = drops.replaced;
            report.overflow = drops.overflow;
        }
        self.frames[f].in_use = self.current.clone();
        let sets = self.frames[f].sets;

        // SAFETY: the command buffer is not in flight (fence waited) and its pool allows reset;
        // every recorded handle is owned here or by the held picture.
        unsafe {
            self.dev
                .device
                .reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty())?;
            self.dev.device.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            if let Some(t) = &mut self.timer {
                t.begin(&self.dev, cmd, f);
            }
            if self.ui_on {
                self.ui.finish(&self.dev, cmd, f);
            }
            self.compose(cmd, self.pipes.pass_out, self.framebuffers[index as usize], sets);
            if let Some(t) = &self.timer {
                t.end(&self.dev, cmd, f);
            }
            self.dev.device.end_command_buffer(cmd)?;
        }
        let waits = [ready];
        let stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
        let signals = [self.done[index as usize]];
        let cmds = [cmd];
        let mut submit = vk::SubmitInfo::default().command_buffers(&cmds);
        if presents {
            submit = submit
                .wait_semaphores(&waits)
                .wait_dst_stage_mask(&stages)
                .signal_semaphores(&signals);
        }
        {
            let _q = self.dev.vkd.queue_lock.guard();
            // SAFETY: the fence is signalled (waited above) and reset here; the submit's borrows
            // outlive the call; the queue is held under the shared lock.
            unsafe {
                self.dev.device.reset_fences(&[fence])?;
                self.dev.device.queue_submit(self.dev.queue, &[submit], fence)?;
            }
            let presenting = Instant::now();
            let fresh = self.output.present(self.dev.queue, index, signals[0])?;
            if presents {
                self.pace.presented(presenting.elapsed());
            }
            if !fresh {
                self.stale = true;
            }
        }
        if let Some(turn) = self.current.as_ref().and_then(|p| p.turn) {
            self.dev.vkd.queue_lock.end_present_turn(turn);
        }
        if !presents {
            // Nothing paces an offscreen target: hold the loop to the nominal refresh.
            if let Some(last) = self.last_present {
                if let Some(rest) = period.checked_sub(last.elapsed()) {
                    std::thread::sleep(rest);
                }
            }
        }
        let now = Instant::now();
        report.missed = self.last_present.is_some_and(|t| now - t > period + period / 2);
        self.last_present = Some(now);
        if let Some(line) = self.pace.close(now).filter(|_| presents) {
            tracing::info!("{line}");
        }
        if report.new_picture {
            if let Some(p) = &self.current {
                note_displayed(p);
            }
        }
        self.frame = (f + 1) % FRAMES;
        Ok(report)
    }

    fn set_stream(&mut self, on: bool, fit: Fit) {
        self.stream = on.then_some(fit);
        self.pace.reset_gpu();
        if !on {
            self.slot.clear();
            self.current = None;
            self.ui_on = true;
        }
    }

    fn set_ui_layer(&mut self, on: bool) {
        self.ui_on = on;
    }

    fn read_back(&mut self) -> Result<Option<Rgba>> {
        self.ensure_readback()?;
        self.wait_idle();
        let rb = self.readback.as_ref().expect("just ensured");
        let extent = self.output.extent;
        let (framebuffer, image, buffer, ptr) = (rb.framebuffer, rb.image, rb.buffer, rb.ptr);
        let cmd = self.frames[self.frame].cmd;
        // SAFETY: the device is idle, so the command buffer and the read-back set are free; the
        // fence below is local and destroyed after its wait.
        unsafe {
            self.dev
                .device
                .reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty())?;
            self.dev.device.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            self.compose(cmd, self.pipes.pass_read, framebuffer, self.readback_sets);
            let region = [vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    layer_count: 1,
                    ..Default::default()
                })
                .image_extent(vk::Extent3D {
                    width: extent.width,
                    height: extent.height,
                    depth: 1,
                })];
            self.dev.device.cmd_copy_image_to_buffer(
                cmd,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                buffer,
                &region,
            );
            self.dev.device.end_command_buffer(cmd)?;
            let fence = self.dev.device.create_fence(&vk::FenceCreateInfo::default(), None)?;
            let cmds = [cmd];
            {
                let _q = self.dev.vkd.queue_lock.guard();
                self.dev.device.queue_submit(
                    self.dev.queue,
                    &[vk::SubmitInfo::default().command_buffers(&cmds)],
                    fence,
                )?;
            }
            let waited = self.dev.device.wait_for_fences(&[fence], true, 5_000_000_000);
            self.dev.device.destroy_fence(fence, None);
            waited.context("read-back fence")?;
        }
        let len = extent.width as usize * extent.height as usize * 4;
        let mut pixels = vec![0u8; len];
        // SAFETY: `ptr` maps `len` bytes of host-coherent memory the copy above finished writing.
        unsafe { std::ptr::copy_nonoverlapping(ptr, pixels.as_mut_ptr(), len) };
        if self.output.format == vk::Format::B8G8R8A8_UNORM {
            for px in pixels.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        }
        Ok(Some(Rgba {
            width: extent.width,
            height: extent.height,
            pixels,
        }))
    }
}

/// The overlay's display stage: decoded → handed to scan-out, on the session's HUD, and the
/// capture-to-glass time the audio's A/V sync compares against.
fn note_displayed(picture: &Picture) {
    let now = u64::try_from(punktfunk_core::client::now_realtime_ns()).unwrap_or(0);
    if let Some(hud) = &picture.hud {
        hud.note_displayed(picture.pts_ns, picture.decoded_ns, now, now);
    }
    if let Some(e2e) = &picture.e2e {
        e2e.note(picture.pts_ns, now);
    }
}

impl Drop for Presenter {
    fn drop(&mut self) {
        self.wait_idle();
        for fr in &mut self.frames {
            fr.in_use = None;
        }
        self.current = None;
        let dev = &self.dev;
        // SAFETY: the device is idle and every handle below was created by this presenter.
        unsafe {
            if let Some(rb) = self.readback.take() {
                destroy_readback(dev, &rb);
            }
            self.ui.destroy(dev);
            if let Some(t) = &self.timer {
                t.destroy(dev);
            }
            for fr in self.frames.drain(..) {
                dev.device.destroy_fence(fr.fence, None);
                dev.device.destroy_semaphore(fr.ready, None);
            }
            for sem in self.done.drain(..) {
                dev.device.destroy_semaphore(sem, None);
            }
            for fb in self.framebuffers.drain(..) {
                dev.device.destroy_framebuffer(fb, None);
            }
            dev.device.destroy_descriptor_pool(self.desc_pool, None);
            dev.device.destroy_command_pool(self.pool, None);
            self.pipes.destroy(dev);
            self.output.destroy(dev);
        }
    }
}

/// # Safety
/// The device is idle and `rb` was built on it; nothing uses `rb` afterwards.
unsafe fn destroy_readback(dev: &GpuDevice, rb: &Readback) {
    // SAFETY: forwarded from the caller.
    unsafe {
        dev.device.destroy_framebuffer(rb.framebuffer, None);
        dev.device.destroy_image_view(rb.view, None);
        dev.device.destroy_image(rb.image, None);
        dev.device.free_memory(rb.memory, None);
        dev.device.destroy_buffer(rb.buffer, None);
        dev.device.free_memory(rb.buffer_memory, None);
    }
}

/// One planar, two-plane, overlay and tiled set from `pool`, freed with it.
fn alloc_sets(dev: &GpuDevice, pool: vk::DescriptorPool, pipes: &Pipelines) -> Result<[vk::DescriptorSet; 4]> {
    let layouts = [
        pipes.planar.set_layout,
        pipes.two_plane.set_layout,
        pipes.overlay.set_layout,
        pipes.tiled.set_layout,
    ];
    // SAFETY: the pool and layouts are live; the sets are freed with the pool.
    let sets = unsafe {
        dev.device.allocate_descriptor_sets(
            &vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(pool)
                .set_layouts(&layouts),
        )
    }?;
    Ok([sets[0], sets[1], sets[2], sets[3]])
}

fn semaphore(dev: &GpuDevice) -> Result<vk::Semaphore> {
    // SAFETY: the create-info is a local; the semaphore is destroyed by its owner.
    Ok(unsafe { dev.device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }?)
}

fn framebuffer(
    dev: &GpuDevice,
    pass: vk::RenderPass,
    view: vk::ImageView,
    extent: vk::Extent2D,
) -> Result<vk::Framebuffer> {
    let attachments = [view];
    // SAFETY: the builder and the view it borrows outlive the call; the owner destroys it.
    Ok(unsafe {
        dev.device.create_framebuffer(
            &vk::FramebufferCreateInfo::default()
                .render_pass(pass)
                .attachments(&attachments)
                .width(extent.width)
                .height(extent.height)
                .layers(1),
            None,
        )
    }?)
}
