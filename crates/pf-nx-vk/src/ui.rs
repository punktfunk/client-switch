//! The UI layer: what the console and the stream overlays draw, as one premultiplied image the
//! presenter blends over the picture.
//!
//! Two ways to fill it. With `skia-vulkan` Skia draws on the shared device into an image this
//! crate samples. Without it Skia rasters on the CPU and each frame is copied up through a
//! staging buffer; above 1080 rows that raster runs at half size and the sampler scales it.

use anyhow::{anyhow, Result};
use ash::vk;

use crate::device::GpuDevice;

/// Raster above this height draws at half size: 4K of CPU raster per frame is too slow.
const RASTER_MAX_HEIGHT: u32 = 1080;

/// Frames in flight; one staging buffer each.
pub const FRAMES: usize = 2;

pub enum UiLayer {
    Raster(RasterUi),
    #[cfg(feature = "skia-vulkan")]
    Gpu(GpuUi),
}

impl UiLayer {
    pub fn new(dev: &GpuDevice, output: vk::Extent2D) -> Result<Self> {
        #[cfg(feature = "skia-vulkan")]
        let fallback = match GpuUi::new(dev, output) {
            Ok(gpu) => return Ok(Self::Gpu(gpu)),
            Err(e) => {
                tracing::warn!("ui: Skia on Vulkan unavailable, rastering on the CPU: {e:#}");
                Some(format!("{e:#}"))
            }
        };
        #[cfg(not(feature = "skia-vulkan"))]
        let fallback = None;
        let mut raster = RasterUi::new(dev, output)?;
        raster.fallback = fallback;
        Ok(Self::Raster(raster))
    }

    /// How the layer draws, for the startup log: its backend and size, and why it fell back.
    /// The console builds this before logging starts, so its own lines never reach the log.
    pub fn describe(&self) -> String {
        let vk::Extent2D { width, height } = self.extent();
        match self {
            Self::Raster(r) => match &r.fallback {
                Some(why) => format!("CPU raster {width}x{height} (Skia on Vulkan: {why})"),
                None => format!("CPU raster {width}x{height}"),
            },
            #[cfg(feature = "skia-vulkan")]
            Self::Gpu(_) => format!("Skia on Vulkan {width}x{height}"),
        }
    }

    pub fn extent(&self) -> vk::Extent2D {
        match self {
            Self::Raster(r) => r.extent,
            #[cfg(feature = "skia-vulkan")]
            Self::Gpu(g) => g.extent,
        }
    }

    pub fn view(&self) -> vk::ImageView {
        match self {
            Self::Raster(r) => r.view,
            #[cfg(feature = "skia-vulkan")]
            Self::Gpu(g) => g.view,
        }
    }

    /// Clears the layer to transparent and hands its canvas to `draw`.
    pub fn draw(&mut self, draw: &mut dyn FnMut(&skia_safe::Canvas)) {
        let surface = match self {
            Self::Raster(r) => &mut r.surface,
            #[cfg(feature = "skia-vulkan")]
            Self::Gpu(g) => &mut g.surface,
        };
        let canvas = surface.canvas();
        canvas.clear(skia_safe::Color::TRANSPARENT);
        draw(canvas);
    }

    /// Makes the drawn layer readable as `SHADER_READ_ONLY` before `cmd` samples it: the
    /// raster copy recorded into `cmd`, or Skia's own flush on the queue.
    ///
    /// # Safety
    /// `cmd` is recording on `dev`, and the frame slot `frame` is not in flight.
    pub unsafe fn finish(&mut self, dev: &GpuDevice, cmd: vk::CommandBuffer, frame: usize) {
        match self {
            // SAFETY: forwarded from the caller.
            Self::Raster(r) => unsafe { r.upload(dev, cmd, frame) },
            #[cfg(feature = "skia-vulkan")]
            Self::Gpu(g) => g.flush(dev),
        }
    }

    /// # Safety
    /// The device is idle.
    pub unsafe fn destroy(&mut self, dev: &GpuDevice) {
        match self {
            // SAFETY: forwarded from the caller.
            Self::Raster(r) => unsafe { r.destroy(dev) },
            #[cfg(feature = "skia-vulkan")]
            // SAFETY: forwarded from the caller.
            Self::Gpu(g) => unsafe { g.destroy(dev) },
        }
    }
}

pub struct RasterUi {
    surface: skia_safe::Surface,
    extent: vk::Extent2D,
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    staging: Vec<(vk::Buffer, vk::DeviceMemory, *mut u8)>,
    /// The image has been written once, so its contents are `SHADER_READ_ONLY`.
    written: bool,
    /// Why Skia on Vulkan was not used, when it was tried.
    fallback: Option<String>,
}

// SAFETY: the mapped pointers address memory this struct owns for its whole life and only this
// struct writes through them, on whichever single thread holds it.
unsafe impl Send for RasterUi {}

impl RasterUi {
    fn new(dev: &GpuDevice, output: vk::Extent2D) -> Result<Self> {
        let extent = if output.height > RASTER_MAX_HEIGHT {
            vk::Extent2D {
                width: output.width / 2,
                height: output.height / 2,
            }
        } else {
            output
        };
        let info = skia_safe::ImageInfo::new(
            (extent.width as i32, extent.height as i32),
            skia_safe::ColorType::BGRA8888,
            skia_safe::AlphaType::Premul,
            None,
        );
        let surface = skia_safe::surfaces::raster(&info, None, None).ok_or_else(|| anyhow!("raster UI surface"))?;
        let (image, memory) = crate::util::image(
            dev,
            extent,
            vk::Format::B8G8R8A8_UNORM,
            vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )?;
        let view = crate::util::view(dev, image, vk::Format::B8G8R8A8_UNORM)?;
        let bytes = u64::from(extent.width) * u64::from(extent.height) * 4;
        let staging = (0..FRAMES)
            .map(|_| crate::util::host_buffer(dev, bytes, vk::BufferUsageFlags::TRANSFER_SRC))
            .collect::<Result<Vec<_>>>()?;
        tracing::info!("ui: CPU raster at {}x{}", extent.width, extent.height);
        Ok(Self {
            surface,
            extent,
            image,
            memory,
            view,
            staging,
            written: false,
            fallback: None,
        })
    }

    /// # Safety
    /// `cmd` is recording and staging buffer `frame` is not read by work in flight.
    unsafe fn upload(&mut self, dev: &GpuDevice, cmd: vk::CommandBuffer, frame: usize) {
        let (buffer, _, ptr) = self.staging[frame];
        let Some(pixels) = self.surface.peek_pixels() else {
            return;
        };
        let Some(bytes) = pixels.bytes() else { return };
        let row = self.extent.width as usize * 4;
        let rows = self.extent.height as usize;
        // SAFETY: `ptr` maps `row * rows` bytes this struct owns; the frame's previous copy out
        // of it finished (the caller waited its fence). Rows are copied one by one because the
        // raster's row stride may exceed the tight width.
        unsafe {
            for y in 0..rows {
                let src = &bytes[y * pixels.row_bytes()..y * pixels.row_bytes() + row];
                std::ptr::copy_nonoverlapping(src.as_ptr(), ptr.add(y * row), row);
            }
        }
        let from = if self.written {
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
        } else {
            vk::ImageLayout::UNDEFINED
        };
        // SAFETY: `cmd` is recording (caller) and every handle is owned here.
        unsafe {
            crate::util::barrier(
                dev,
                cmd,
                self.image,
                from,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                (vk::PipelineStageFlags2::FRAGMENT_SHADER, vk::AccessFlags2::SHADER_READ),
                (vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_WRITE),
            );
            let region = [vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    layer_count: 1,
                    ..Default::default()
                })
                .image_extent(vk::Extent3D {
                    width: self.extent.width,
                    height: self.extent.height,
                    depth: 1,
                })];
            dev.device.cmd_copy_buffer_to_image(
                cmd,
                buffer,
                self.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &region,
            );
            crate::util::barrier(
                dev,
                cmd,
                self.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                (vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_WRITE),
                (vk::PipelineStageFlags2::FRAGMENT_SHADER, vk::AccessFlags2::SHADER_READ),
            );
        }
        self.written = true;
    }

    /// # Safety
    /// The device is idle.
    unsafe fn destroy(&mut self, dev: &GpuDevice) {
        // SAFETY: the caller guarantees the device is idle; every handle is owned here.
        unsafe {
            dev.device.destroy_image_view(self.view, None);
            dev.device.destroy_image(self.image, None);
            dev.device.free_memory(self.memory, None);
            for (buffer, memory, _) in self.staging.drain(..) {
                dev.device.destroy_buffer(buffer, None);
                dev.device.free_memory(memory, None);
            }
        }
    }
}

#[cfg(feature = "skia-vulkan")]
pub use gpu::GpuUi;

#[cfg(feature = "skia-vulkan")]
mod gpu {
    use anyhow::{anyhow, Context, Result};
    use ash::vk::{self, Handle as _};
    use skia_safe::gpu::{self, vk as skvk, DirectContext, SurfaceOrigin};

    use crate::device::GpuDevice;

    /// Skia on the shared device, drawing into a render target the presenter samples.
    pub struct GpuUi {
        pub(super) context: DirectContext,
        pub(super) surface: skia_safe::Surface,
        pub(super) view: vk::ImageView,
        pub(super) extent: vk::Extent2D,
        qf: u32,
    }

    impl GpuUi {
        pub(super) fn new(dev: &GpuDevice, extent: vk::Extent2D) -> Result<Self> {
            let entry = dev.entry.clone();
            let instance = dev.instance.clone();
            let get_proc = move |of: skvk::GetProcOf| -> *const std::ffi::c_void {
                // SAFETY: Skia passes back the raw handles it was given below, which `dev` owns
                // for longer than this context lives; each name is a NUL-terminated C string.
                unsafe {
                    match of {
                        skvk::GetProcOf::Instance(raw, name) => entry
                            .get_instance_proc_addr(vk::Instance::from_raw(raw as _), name)
                            .map_or(std::ptr::null(), |f| f as *const std::ffi::c_void),
                        skvk::GetProcOf::Device(raw, name) => {
                            (instance.fp_v1_0().get_device_proc_addr)(vk::Device::from_raw(raw as _), name)
                                .map_or(std::ptr::null(), |f| f as *const std::ffi::c_void)
                        }
                    }
                }
            };
            let builder = skvk::BackendContext::new_builder(
                dev.instance.handle().as_raw() as _,
                dev.pdev.as_raw() as _,
                dev.device.handle().as_raw() as _,
                (dev.queue.as_raw() as _, dev.qf as usize),
                &get_proc,
                Some(skvk::Version::from(crate::device::INSTANCE_API_VERSION)),
            );
            // SAFETY: the handles belong to `dev`, which outlives this context; Skia does not
            // take ownership of them.
            let backend = unsafe { builder.build() };
            let mut context =
                gpu::direct_contexts::make_vulkan(&backend, None).ok_or_else(|| anyhow!("Skia DirectContext"))?;
            // The kit's floor: GPU memory comes out of the title's heap, which applet mode shrinks.
            context.set_resource_cache_limit(96 << 20);
            let info = skia_safe::ImageInfo::new(
                (extent.width as i32, extent.height as i32),
                skia_safe::ColorType::BGRA8888,
                skia_safe::AlphaType::Premul,
                None,
            );
            let mut surface = gpu::surfaces::render_target(
                &mut context,
                gpu::Budgeted::Yes,
                &info,
                None,
                SurfaceOrigin::TopLeft,
                None,
                false,
                None,
            )
            .context("Skia render target")?;
            let texture =
                gpu::surfaces::get_backend_texture(&mut surface, skia_safe::surface::BackendHandleAccess::FlushRead)
                    .context("UI backend texture")?;
            let image_info = texture.vulkan_image_info().context("UI texture is not Vulkan")?;
            let image = vk::Image::from_raw(*image_info.image() as u64);
            let view = crate::util::view(dev, image, vk::Format::from_raw(image_info.format as i32))?;
            tracing::info!("ui: Skia on Vulkan at {}x{}", extent.width, extent.height);
            Ok(Self {
                context,
                surface,
                view,
                extent,
                qf: dev.qf,
            })
        }

        /// Submits Skia's work, ending in `SHADER_READ_ONLY` on the queue the presenter uses.
        pub(super) fn flush(&mut self, dev: &GpuDevice) {
            let _q = dev.vkd.queue_lock.guard();
            self.context.flush_surface_with_texture_state(
                &mut self.surface,
                &gpu::FlushInfo::default(),
                Some(&skvk::mutable_texture_states::new_vulkan(
                    skvk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    self.qf,
                )),
            );
            self.context.submit(None);
        }

        /// # Safety
        /// The device is idle.
        pub(super) unsafe fn destroy(&mut self, dev: &GpuDevice) {
            // SAFETY: the caller guarantees the device is idle; the view is ours, the image
            // Skia's, freed with the surface.
            unsafe { dev.device.destroy_image_view(self.view, None) };
        }
    }
}
