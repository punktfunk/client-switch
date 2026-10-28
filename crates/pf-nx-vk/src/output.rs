//! Where frames go: a FIFO swapchain on the console's `NWindow` (`VK_NN_vi_surface`), or
//! offscreen images on a box without a window. Both hand the presenter an image index to render
//! into.

use anyhow::{anyhow, Context, Result};
use ash::vk;

use crate::device::{GpuDevice, Target};

/// The formats the presenter draws in, first offered wins. The CSC writes non-linear values, so
/// an `_SRGB` view would apply the transfer twice.
const FORMATS: [vk::Format; 2] = [vk::Format::B8G8R8A8_UNORM, vk::Format::R8G8B8A8_UNORM];

/// The panel and the dock both scan out at 60 Hz (plan D2).
// TODO(verify): whether either runs at 59.94 Hz; the pacing record of a first stream says.
const REFRESH_MHZ: u32 = 60_000;

enum Kind {
    Swapchain {
        surface_i: ash::khr::surface::Instance,
        swap_d: ash::khr::swapchain::Device,
        surface: vk::SurfaceKHR,
        swapchain: vk::SwapchainKHR,
    },
    Offscreen {
        memory: Vec<vk::DeviceMemory>,
        next: u32,
    },
}

pub struct Output {
    kind: Kind,
    pub extent: vk::Extent2D,
    pub refresh_mhz: u32,
    pub format: vk::Format,
    pub images: Vec<vk::Image>,
    pub views: Vec<vk::ImageView>,
    /// The layout a finished frame leaves the image in.
    pub final_layout: vk::ImageLayout,
}

impl Output {
    /// An output of `want` pixels, where the target lets the caller choose.
    pub fn new(dev: &GpuDevice, want: vk::Extent2D) -> Result<Self> {
        match dev.target {
            Target::Vi { window, .. } => Self::vi(dev, window, want),
            Target::Offscreen { width, height } => Self::offscreen(dev, width, height),
        }
    }

    fn vi(dev: &GpuDevice, window: *mut std::ffi::c_void, want: vk::Extent2D) -> Result<Self> {
        let vi_i = ash::nn::vi_surface::Instance::new(&dev.entry, &dev.instance);
        // SAFETY: `window` is libnx's default `NWindow`, live for the process; the surface is
        // destroyed in `destroy`.
        let surface = unsafe { vi_i.create_vi_surface(&vk::ViSurfaceCreateInfoNN::default().window(window), None) }
            .context("vi surface")?;
        let surface_i = ash::khr::surface::Instance::new(&dev.entry, &dev.instance);
        let swap_d = ash::khr::swapchain::Device::new(&dev.instance, &dev.device);
        match Self::swapchain(dev, &surface_i, &swap_d, surface, want) {
            Ok((swapchain, extent, format, images, views)) => Ok(Self {
                kind: Kind::Swapchain {
                    surface_i,
                    swap_d,
                    surface,
                    swapchain,
                },
                extent,
                refresh_mhz: REFRESH_MHZ,
                format,
                images,
                views,
                final_layout: vk::ImageLayout::PRESENT_SRC_KHR,
            }),
            Err(e) => {
                // SAFETY: nothing was built on the surface.
                unsafe { surface_i.destroy_surface(surface, None) };
                Err(e)
            }
        }
    }

    #[allow(clippy::type_complexity)]
    fn swapchain(
        dev: &GpuDevice,
        surface_i: &ash::khr::surface::Instance,
        swap_d: &ash::khr::swapchain::Device,
        surface: vk::SurfaceKHR,
        want: vk::Extent2D,
    ) -> Result<(
        vk::SwapchainKHR,
        vk::Extent2D,
        vk::Format,
        Vec<vk::Image>,
        Vec<vk::ImageView>,
    )> {
        // SAFETY: live physical device and surface.
        let caps = unsafe { surface_i.get_physical_device_surface_capabilities(dev.pdev, surface) }?;
        // TODO(verify): whether NVK reports the window's size here or leaves it to us
        // (u32::MAX); docked 1920x1080 needs the latter or the NWindow resized first.
        let extent = if caps.current_extent.width == u32::MAX {
            vk::Extent2D {
                width: want
                    .width
                    .clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                height: want
                    .height
                    .clamp(caps.min_image_extent.height, caps.max_image_extent.height),
            }
        } else {
            caps.current_extent
        };
        let mut count = 3.max(caps.min_image_count);
        if caps.max_image_count > 0 {
            count = count.min(caps.max_image_count);
        }
        // SAFETY: as above.
        let formats = unsafe { surface_i.get_physical_device_surface_formats(dev.pdev, surface) }?;
        let format = FORMATS
            .iter()
            .find_map(|&ours| {
                formats
                    .iter()
                    .find(|f| f.format == ours && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR)
            })
            .ok_or_else(|| anyhow!("the vi surface offers no 8-bit UNORM format: {formats:?}"))?;
        let ci = vk::SwapchainCreateInfoKHR::default()
            .surface(surface)
            .min_image_count(count)
            .image_format(format.format)
            .image_color_space(format.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
            .present_mode(vk::PresentModeKHR::FIFO)
            .clipped(true);
        // SAFETY: `ci` outlives the call; the swapchain is destroyed in `destroy`.
        let swapchain = unsafe { swap_d.create_swapchain(&ci, None) }.context("vi swapchain")?;
        // SAFETY: the swapchain is live; its images belong to it.
        let images = unsafe { swap_d.get_swapchain_images(swapchain) }?;
        let views = views(dev, &images, format.format)?;
        tracing::info!(
            "swapchain {}x{} {:?}, {} images, FIFO, transform {:?}",
            extent.width,
            extent.height,
            format.format,
            images.len(),
            caps.current_transform
        );
        Ok((swapchain, extent, format.format, images, views))
    }

    fn offscreen(dev: &GpuDevice, width: u32, height: u32) -> Result<Self> {
        let extent = vk::Extent2D { width, height };
        let mut images = Vec::new();
        let mut memory = Vec::new();
        for _ in 0..2 {
            let (image, mem) = crate::util::image(
                dev,
                extent,
                FORMATS[0],
                vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )?;
            images.push(image);
            memory.push(mem);
        }
        let views = views(dev, &images, FORMATS[0])?;
        Ok(Self {
            kind: Kind::Offscreen { memory, next: 0 },
            extent,
            refresh_mhz: REFRESH_MHZ,
            format: FORMATS[0],
            images,
            views,
            final_layout: vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
        })
    }

    /// Whether frames reach a display, so a present waits on a semaphore.
    pub fn presents(&self) -> bool {
        matches!(self.kind, Kind::Swapchain { .. })
    }

    /// The next image to render into, or `None` when the swapchain no longer matches the
    /// window and must be rebuilt. A swapchain signals `ready` when the image may be written.
    pub fn acquire(&mut self, ready: vk::Semaphore) -> Result<Option<u32>> {
        match &mut self.kind {
            Kind::Swapchain { swap_d, swapchain, .. } => {
                // SAFETY: `ready` is unsignalled: its previous signal was consumed by the submit
                // whose fence the presenter waited before calling this, and a failed acquire
                // leaves it untouched.
                match unsafe { swap_d.acquire_next_image(*swapchain, u64::MAX, ready, vk::Fence::null()) } {
                    Ok((index, _suboptimal)) => Ok(Some(index)),
                    Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => Ok(None),
                    Err(e) => Err(anyhow!("acquire_next_image: {e}")),
                }
            }
            Kind::Offscreen { next, .. } => {
                let index = *next;
                *next = (*next + 1) % 2;
                Ok(Some(index))
            }
        }
    }

    /// Queues image `index` for scan-out once `done` signals. Hold the queue lock. `false` when
    /// the swapchain is out of date; a suboptimal one is kept, so a driver that always says so
    /// cannot make every frame a rebuild.
    pub fn present(&self, queue: vk::Queue, index: u32, done: vk::Semaphore) -> Result<bool> {
        let Kind::Swapchain { swap_d, swapchain, .. } = &self.kind else {
            return Ok(true);
        };
        let waits = [done];
        let swapchains = [*swapchain];
        let indices = [index];
        let info = vk::PresentInfoKHR::default()
            .wait_semaphores(&waits)
            .swapchains(&swapchains)
            .image_indices(&indices);
        // SAFETY: the caller holds the queue lock; every handle is live and owned here.
        match unsafe { swap_d.queue_present(queue, &info) } {
            Ok(_suboptimal) => Ok(true),
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => Ok(false),
            Err(e) => Err(anyhow!("queue_present: {e}")),
        }
    }

    /// Destroys everything this output owns; a second call does nothing. The device must be idle.
    ///
    /// # Safety
    /// `dev` is the device this output was built on, and no queued work uses it.
    pub unsafe fn destroy(&mut self, dev: &GpuDevice) {
        // SAFETY: the caller guarantees the device is idle and owns these handles.
        unsafe {
            for view in self.views.drain(..) {
                dev.device.destroy_image_view(view, None);
            }
            match &mut self.kind {
                Kind::Swapchain {
                    surface_i,
                    swap_d,
                    surface,
                    swapchain,
                } => {
                    swap_d.destroy_swapchain(std::mem::take(swapchain), None);
                    surface_i.destroy_surface(std::mem::take(surface), None);
                }
                Kind::Offscreen { memory, .. } => {
                    for image in self.images.drain(..) {
                        dev.device.destroy_image(image, None);
                    }
                    for mem in memory.drain(..) {
                        dev.device.free_memory(mem, None);
                    }
                }
            }
        }
    }
}

fn views(dev: &GpuDevice, images: &[vk::Image], format: vk::Format) -> Result<Vec<vk::ImageView>> {
    images
        .iter()
        .map(|&image| crate::util::view(dev, image, format))
        .collect()
}
