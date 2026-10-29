//! Check 4's second half: a FIFO swapchain on the default `NWindow` (`VK_NN_vi_surface`), each
//! frame cleared to a moving colour and one textured quad drawn with the presenter's own shaders
//! (`pf_client_video::video_csc_spv`: the fullscreen triangle and the overlay blit) into a
//! viewport that slides across the screen. 600 frames, then the frame-time histogram, the
//! swapchain and the GPU clock before and after.
//!
//! The text console holds the same window through libnx's framebuffer, so it is closed for the
//! frames and opened again after: what the probe prints meanwhile goes to the log file only.

use std::time::{Duration, Instant};

use ash::vk;
use pf_client_video::video_csc_spv as spv;
use pf_nx_sys::{clk, vi};

use crate::vk::{at, Fail};
use crate::{nx, Out, Steps};

const FRAMES: usize = 600;
const IN_FLIGHT: usize = 2;
const TEX: u32 = 256;
/// One second: the panel refreshes 60 times in it.
const WAIT_NS: u64 = 1_000_000_000;
/// The presenter's formats, first offered wins.
const FORMATS: [vk::Format; 2] = [vk::Format::B8G8R8A8_UNORM, vk::Format::R8G8B8A8_UNORM];
const COLOR: vk::ImageSubresourceRange = vk::ImageSubresourceRange {
    aspect_mask: vk::ImageAspectFlags::COLOR,
    base_mip_level: 0,
    level_count: 1,
    base_array_layer: 0,
    layer_count: 1,
};

/// What check 4's first half made, for this half to build on.
pub struct Gpu<'a> {
    pub entry: &'a ash::Entry,
    pub instance: &'a ash::Instance,
    pub pdev: vk::PhysicalDevice,
    pub device: &'a ash::Device,
    pub family: u32,
    pub queue: vk::Queue,
}

/// Every object this half makes, null until made. Destroying a null handle is a no-op in
/// Vulkan, so one teardown covers a run that stopped anywhere.
#[derive(Default)]
struct Objects {
    surface: vk::SurfaceKHR,
    swapchain: vk::SwapchainKHR,
    views: Vec<vk::ImageView>,
    pass: vk::RenderPass,
    framebuffers: Vec<vk::Framebuffer>,
    staging: vk::Buffer,
    staging_mem: vk::DeviceMemory,
    texture: vk::Image,
    texture_mem: vk::DeviceMemory,
    texture_view: vk::ImageView,
    sampler: vk::Sampler,
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    descriptors: vk::DescriptorPool,
    set: vk::DescriptorSet,
    commands: vk::CommandPool,
    ready: Vec<vk::Semaphore>,
    done: Vec<vk::Semaphore>,
    fences: Vec<vk::Fence>,
}

pub fn steps(gpu: &Gpu, steps: &mut Steps, out: &mut Out) {
    let before = clk::clocks();
    nx::screen_pause();
    out.note("probe 4 vulkan at surface");
    let surface_i = ash::khr::surface::Instance::new(gpu.entry, gpu.instance);
    let swap_d = ash::khr::swapchain::Device::new(gpu.instance, gpu.device);
    let mut objects = Objects::default();
    let result = run(gpu, &surface_i, &swap_d, &mut objects, steps, out);
    let after = clk::clocks();
    // SAFETY: every queued frame is waited for before the objects go; each handle is this
    // run's own or null.
    unsafe { teardown(gpu.device, &surface_i, &swap_d, &mut objects) };
    nx::screen_resume();
    if let Err((step, why)) = result {
        steps.push((step, Err(why)));
    }
    let mhz = |c: Option<clk::Clocks>| match c {
        Some(c) => format!("gpu{}/cpu{}/emc{}MHz", c.gpu_hz / 1_000_000, c.cpu_hz / 1_000_000, c.emc_hz / 1_000_000),
        None => "?".into(),
    };
    steps.push(("clock", Ok(format!("{}->{}", mhz(before), mhz(after)))));
}

fn run(
    gpu: &Gpu,
    surface_i: &ash::khr::surface::Instance,
    swap_d: &ash::khr::swapchain::Device,
    o: &mut Objects,
    steps: &mut Steps,
    out: &mut Out,
) -> Result<(), Fail> {
    let d = gpu.device;
    let started = Instant::now();
    let window = vi::default_window();
    let vi_i = ash::nn::vi_surface::Instance::new(gpu.entry, gpu.instance);
    // SAFETY: `window` is libnx's default `NWindow`, live for the process; the console let go
    // of it in `screen_pause`.
    o.surface = unsafe { vi_i.create_vi_surface(&vk::ViSurfaceCreateInfoNN::default().window(window), None) }
        .map_err(at("surface"))?;
    // SAFETY: live physical device and surface.
    let (support, caps, formats) = unsafe {
        (
            surface_i
                .get_physical_device_surface_support(gpu.pdev, gpu.family, o.surface)
                .map_err(at("surface"))?,
            surface_i
                .get_physical_device_surface_capabilities(gpu.pdev, o.surface)
                .map_err(at("surface"))?,
            surface_i
                .get_physical_device_surface_formats(gpu.pdev, o.surface)
                .map_err(at("surface"))?,
        )
    };
    if !support {
        return Err(("surface", format!("family{} cannot present", gpu.family)));
    }
    let (win_w, win_h) = vi::window_size(window);
    steps.push((
        "surface",
        Ok(format!(
            "window={win_w}x{win_h} current={}x{} images={}..{}",
            caps.current_extent.width, caps.current_extent.height, caps.min_image_count, caps.max_image_count
        )),
    ));

    let extent = if caps.current_extent.width == u32::MAX {
        vk::Extent2D {
            width: if win_w > 0 { win_w } else { 1280 },
            height: if win_h > 0 { win_h } else { 720 },
        }
    } else {
        caps.current_extent
    };
    let format = FORMATS
        .iter()
        .find_map(|&ours| {
            formats
                .iter()
                .find(|f| f.format == ours && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR)
        })
        .ok_or_else(|| ("swapchain", format!("no 8-bit UNORM format in {formats:?}")))?;
    let mut count = 3.max(caps.min_image_count);
    if caps.max_image_count > 0 {
        count = count.min(caps.max_image_count);
    }
    let info = vk::SwapchainCreateInfoKHR::default()
        .surface(o.surface)
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
    // SAFETY: `info` outlives the call; the swapchain's images belong to it.
    let images = unsafe {
        o.swapchain = swap_d.create_swapchain(&info, None).map_err(at("swapchain"))?;
        swap_d.get_swapchain_images(o.swapchain).map_err(at("swapchain"))?
    };
    steps.push((
        "swapchain",
        Ok(format!(
            "{}x{} {:?} {}img FIFO",
            extent.width,
            extent.height,
            format.format,
            images.len()
        )),
    ));

    out.note("probe 4 vulkan at pipeline");
    for &image in &images {
        o.views.push(view(d, image, format.format)?);
    }
    o.pass = render_pass(d, format.format)?;
    for &v in &o.views {
        let attachments = [v];
        let info = vk::FramebufferCreateInfo::default()
            .render_pass(o.pass)
            .attachments(&attachments)
            .width(extent.width)
            .height(extent.height)
            .layers(1);
        // SAFETY: the view and pass are live; `info` outlives the call.
        o.framebuffers
            .push(unsafe { d.create_framebuffer(&info, None) }.map_err(at("pipeline"))?);
    }
    pipeline(d, o)?;
    // SAFETY: a live device; the pool is destroyed in teardown, its buffers with it.
    let cmds = unsafe {
        o.commands = d
            .create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                    .queue_family_index(gpu.family),
                None,
            )
            .map_err(at("pipeline"))?;
        d.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(o.commands)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(IN_FLIGHT as u32 + 1),
        )
        .map_err(at("pipeline"))?
    };
    steps.push(("pipeline", Ok("fullscreen.vert+overlay.frag".into())));

    out.note("probe 4 vulkan at texture");
    let uploaded = Instant::now();
    texture(gpu, o, cmds[IN_FLIGHT])?;
    steps.push(("texture", Ok(format!("{TEX}x{TEX} upload={}us", uploaded.elapsed().as_micros()))));

    // SAFETY: a live device; teardown destroys them after the queue is idle.
    unsafe {
        for _ in 0..IN_FLIGHT {
            o.ready
                .push(d.create_semaphore(&vk::SemaphoreCreateInfo::default(), None).map_err(at("frames"))?);
            o.fences.push(
                d.create_fence(&vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED), None)
                    .map_err(at("frames"))?,
            );
        }
        for _ in &images {
            o.done
                .push(d.create_semaphore(&vk::SemaphoreCreateInfo::default(), None).map_err(at("frames"))?);
        }
    }

    out.note("probe 4 vulkan at frames");
    let setup = started.elapsed();
    let mut presented = Vec::with_capacity(FRAMES);
    for i in 0..FRAMES {
        frame(gpu, swap_d, o, cmds[i % IN_FLIGHT], i, extent)?;
        presented.push(Instant::now());
        if i % 120 == 119 {
            out.note(&format!("probe 4 vulkan at frames {}", i + 1));
        }
    }
    // SAFETY: a live device.
    unsafe { d.device_wait_idle() }.map_err(at("frames"))?;
    steps.push(("frames", Ok(histogram(setup, &presented))));
    Ok(())
}

fn view(d: &ash::Device, image: vk::Image, format: vk::Format) -> Result<vk::ImageView, Fail> {
    let info = vk::ImageViewCreateInfo::default()
        .image(image)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(format)
        .subresource_range(COLOR);
    // SAFETY: `image` is live; `info` outlives the call.
    unsafe { d.create_image_view(&info, None) }.map_err(at("pipeline"))
}

/// Clear, draw, then hand the image to the presentation engine.
fn render_pass(d: &ash::Device, format: vk::Format) -> Result<vk::RenderPass, Fail> {
    let attachments = [vk::AttachmentDescription::default()
        .format(format)
        .samples(vk::SampleCountFlags::TYPE_1)
        .load_op(vk::AttachmentLoadOp::CLEAR)
        .store_op(vk::AttachmentStoreOp::STORE)
        .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
        .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .final_layout(vk::ImageLayout::PRESENT_SRC_KHR)];
    let refs = [vk::AttachmentReference::default()
        .attachment(0)
        .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
    let subpasses = [vk::SubpassDescription::default()
        .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
        .color_attachments(&refs)];
    // The acquire's semaphore is waited at colour output: the layout change waits there too.
    let deps = [vk::SubpassDependency::default()
        .src_subpass(vk::SUBPASS_EXTERNAL)
        .dst_subpass(0)
        .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
        .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
        .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)];
    let info = vk::RenderPassCreateInfo::default()
        .attachments(&attachments)
        .subpasses(&subpasses)
        .dependencies(&deps);
    // SAFETY: the arrays outlive the call.
    unsafe { d.create_render_pass(&info, None) }.map_err(at("pipeline"))
}

/// The presenter's overlay pipeline: one sampled image at binding 0, no push constants, the
/// bufferless fullscreen triangle scaled into a dynamic viewport.
fn pipeline(d: &ash::Device, o: &mut Objects) -> Result<(), Fail> {
    let words = |bytes: &[u8]| ash::util::read_spv(&mut std::io::Cursor::new(bytes)).map_err(|e| ("pipeline", e.to_string()));
    let (vert_words, frag_words) = (words(spv::FULLSCREEN_VERT)?, words(spv::OVERLAY_FRAG)?);
    // SAFETY: every builder below is a local that outlives its call; the objects land in `o`
    // and the modules are destroyed once the pipeline exists.
    unsafe {
        o.sampler = d
            .create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(vk::Filter::LINEAR)
                    .min_filter(vk::Filter::LINEAR)
                    .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE),
                None,
            )
            .map_err(at("pipeline"))?;
        let immutable = [o.sampler];
        let bindings = [vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT)
            .immutable_samplers(&immutable)];
        o.set_layout = d
            .create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None)
            .map_err(at("pipeline"))?;
        let set_layouts = [o.set_layout];
        o.layout = d
            .create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts), None)
            .map_err(at("pipeline"))?;
        let vert = d
            .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&vert_words), None)
            .map_err(at("pipeline"))?;
        let frag = d.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&frag_words), None);
        let frag = match frag {
            Ok(frag) => frag,
            Err(e) => {
                d.destroy_shader_module(vert, None);
                return Err(("pipeline", format!("{e:?}")));
            }
        };
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vert)
                .name(c"main"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(frag)
                .name(c"main"),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
        let assembly =
            vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        let multisample =
            vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let attachments =
            [vk::PipelineColorBlendAttachmentState::default().color_write_mask(vk::ColorComponentFlags::RGBA)];
        let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&attachments);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
        let info = [vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&assembly)
            .viewport_state(&viewport)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .color_blend_state(&blend)
            .dynamic_state(&dynamic)
            .layout(o.layout)
            .render_pass(o.pass)];
        let made = d.create_graphics_pipelines(vk::PipelineCache::null(), &info, None);
        d.destroy_shader_module(vert, None);
        d.destroy_shader_module(frag, None);
        o.pipeline = made.map_err(|(_, e)| ("pipeline", format!("{e:?}")))?[0];

        let sizes = [vk::DescriptorPoolSize {
            ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            descriptor_count: 1,
        }];
        o.descriptors = d
            .create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default().max_sets(1).pool_sizes(&sizes), None)
            .map_err(at("pipeline"))?;
    }
    Ok(())
}

fn memory_type(gpu: &Gpu, bits: u32, want: vk::MemoryPropertyFlags) -> Option<u32> {
    // SAFETY: a physical device of the live instance.
    let props = unsafe { gpu.instance.get_physical_device_memory_properties(gpu.pdev) };
    props
        .memory_types_as_slice()
        .iter()
        .enumerate()
        .position(|(i, t)| bits & (1 << i) != 0 && t.property_flags.contains(want))
        .map(|i| i as u32)
}

/// A 256×256 RGBA checkerboard over a gradient, written to a host-visible buffer and copied into
/// an optimal-tiled image, as the presenter uploads its planes; then bound at binding 0.
fn texture(gpu: &Gpu, o: &mut Objects, cmd: vk::CommandBuffer) -> Result<(), Fail> {
    let d = gpu.device;
    let bytes = u64::from(TEX * TEX * 4);
    let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
    // SAFETY: every builder is a local outliving its call; the objects land in `o`. The mapping
    // covers `bytes` and is written within it; the submit is waited before returning.
    unsafe {
        o.staging = d
            .create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(bytes)
                    .usage(vk::BufferUsageFlags::TRANSFER_SRC),
                None,
            )
            .map_err(at("texture"))?;
        let needs = d.get_buffer_memory_requirements(o.staging);
        let kind = memory_type(gpu, needs.memory_type_bits, host).ok_or(("texture", "no host memory".to_string()))?;
        o.staging_mem = d
            .allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(needs.size)
                    .memory_type_index(kind),
                None,
            )
            .map_err(at("texture"))?;
        d.bind_buffer_memory(o.staging, o.staging_mem, 0).map_err(at("texture"))?;
        let ptr = d
            .map_memory(o.staging_mem, 0, bytes, vk::MemoryMapFlags::empty())
            .map_err(at("texture"))?
            .cast::<[u8; 4]>();
        for y in 0..TEX {
            for x in 0..TEX {
                let check = ((x / 32) + (y / 32)) % 2 == 0;
                let px = if check {
                    [255, 255, 255, 255]
                } else {
                    [x as u8, y as u8, 255 - x as u8, 255]
                };
                ptr.add((y * TEX + x) as usize).write(px);
            }
        }
        d.unmap_memory(o.staging_mem);

        let extent = vk::Extent3D {
            width: TEX,
            height: TEX,
            depth: 1,
        };
        o.texture = d
            .create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(vk::Format::R8G8B8A8_UNORM)
                    .extent(extent)
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST)
                    .initial_layout(vk::ImageLayout::UNDEFINED),
                None,
            )
            .map_err(at("texture"))?;
        let needs = d.get_image_memory_requirements(o.texture);
        let kind = memory_type(gpu, needs.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)
            .or_else(|| memory_type(gpu, needs.memory_type_bits, vk::MemoryPropertyFlags::empty()))
            .ok_or(("texture", "no image memory".to_string()))?;
        o.texture_mem = d
            .allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(needs.size)
                    .memory_type_index(kind),
                None,
            )
            .map_err(at("texture"))?;
        d.bind_image_memory(o.texture, o.texture_mem, 0).map_err(at("texture"))?;
        o.texture_view = view(d, o.texture, vk::Format::R8G8B8A8_UNORM).map_err(|(_, e)| ("texture", e))?;

        let barrier = |old, new, src, dst| {
            vk::ImageMemoryBarrier::default()
                .old_layout(old)
                .new_layout(new)
                .src_access_mask(src)
                .dst_access_mask(dst)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(o.texture)
                .subresource_range(COLOR)
        };
        d.begin_command_buffer(
            cmd,
            &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )
        .map_err(at("texture"))?;
        d.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier(
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::AccessFlags::empty(),
                vk::AccessFlags::TRANSFER_WRITE,
            )],
        );
        let region = vk::BufferImageCopy::default()
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .image_extent(extent);
        d.cmd_copy_buffer_to_image(
            cmd,
            o.staging,
            o.texture,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[region],
        );
        d.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier(
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::AccessFlags::TRANSFER_WRITE,
                vk::AccessFlags::SHADER_READ,
            )],
        );
        d.end_command_buffer(cmd).map_err(at("texture"))?;
        let cmds = [cmd];
        d.queue_submit(
            gpu.queue,
            &[vk::SubmitInfo::default().command_buffers(&cmds)],
            vk::Fence::null(),
        )
        .map_err(at("texture"))?;
        d.queue_wait_idle(gpu.queue).map_err(at("texture"))?;

        let set_layouts = [o.set_layout];
        let set = d
            .allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(o.descriptors)
                    .set_layouts(&set_layouts),
            )
            .map_err(at("texture"))?[0];
        let images = [vk::DescriptorImageInfo::default()
            .image_view(o.texture_view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let writes = [vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(&images)];
        d.update_descriptor_sets(&writes, &[]);
        o.set = set;
    }
    Ok(())
}

/// Frame `i`: wait for its slot, acquire, clear to a colour that cycles every few seconds, draw
/// the texture into a half-size viewport sliding left and right, submit and present.
fn frame(
    gpu: &Gpu,
    swap_d: &ash::khr::swapchain::Device,
    o: &Objects,
    cmd: vk::CommandBuffer,
    i: usize,
    extent: vk::Extent2D,
) -> Result<(), Fail> {
    let d = gpu.device;
    let slot = i % IN_FLIGHT;
    let fail = |what: &str, e: vk::Result| ("frames", format!("{what} {e:?} at frame {i}"));
    // SAFETY: every handle is live and this run's; the slot's fence guards its command buffer
    // and semaphore, and `done[index]` is waited by the present of the image it signals.
    unsafe {
        d.wait_for_fences(&[o.fences[slot]], true, WAIT_NS)
            .map_err(|e| fail("fence", e))?;
        let (index, _suboptimal) = swap_d
            .acquire_next_image(o.swapchain, WAIT_NS, o.ready[slot], vk::Fence::null())
            .map_err(|e| fail("acquire", e))?;
        d.reset_fences(&[o.fences[slot]]).map_err(|e| fail("fence", e))?;
        d.reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty())
            .map_err(|e| fail("record", e))?;
        d.begin_command_buffer(
            cmd,
            &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )
        .map_err(|e| fail("record", e))?;
        let t = i as f32 / 60.0;
        let wave = |phase: f32| 0.15 + 0.35 * (0.5 + 0.5 * (t + phase).sin());
        let clear = [vk::ClearValue {
            color: vk::ClearColorValue {
                float32: [wave(0.0), wave(2.1), wave(4.2), 1.0],
            },
        }];
        let full = vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent,
        };
        d.cmd_begin_render_pass(
            cmd,
            &vk::RenderPassBeginInfo::default()
                .render_pass(o.pass)
                .framebuffer(o.framebuffers[index as usize])
                .render_area(full)
                .clear_values(&clear),
            vk::SubpassContents::INLINE,
        );
        d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, o.pipeline);
        let (w, h) = (extent.width as f32, extent.height as f32);
        let viewport = vk::Viewport {
            x: w / 4.0 + w / 4.0 * (t * 1.5).sin(),
            y: h / 4.0,
            width: w / 2.0,
            height: h / 2.0,
            min_depth: 0.0,
            max_depth: 1.0,
        };
        d.cmd_set_viewport(cmd, 0, &[viewport]);
        d.cmd_set_scissor(cmd, 0, &[full]);
        d.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::GRAPHICS, o.layout, 0, &[o.set], &[]);
        d.cmd_draw(cmd, 3, 1, 0, 0);
        d.cmd_end_render_pass(cmd);
        d.end_command_buffer(cmd).map_err(|e| fail("record", e))?;

        let waits = [o.ready[slot]];
        let stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
        let cmds = [cmd];
        let signals = [o.done[index as usize]];
        let submit = vk::SubmitInfo::default()
            .wait_semaphores(&waits)
            .wait_dst_stage_mask(&stages)
            .command_buffers(&cmds)
            .signal_semaphores(&signals);
        d.queue_submit(gpu.queue, &[submit], o.fences[slot])
            .map_err(|e| fail("submit", e))?;
        let swapchains = [o.swapchain];
        let indices = [index];
        let present = vk::PresentInfoKHR::default()
            .wait_semaphores(&signals)
            .swapchains(&swapchains)
            .image_indices(&indices);
        swap_d
            .queue_present(gpu.queue, &present)
            .map_err(|e| fail("present", e))?;
    }
    Ok(())
}

/// Frames per second over the run, the present-to-present intervals' median, 99th percentile
/// and worst, and how many fell on one refresh (14–19 ms), two (30–37 ms) or elsewhere.
fn histogram(setup: Duration, presented: &[Instant]) -> String {
    let mut ms: Vec<f64> = presented
        .windows(2)
        .map(|w| (w[1] - w[0]).as_secs_f64() * 1000.0)
        .collect();
    let total: f64 = ms.iter().sum();
    let fps = ms.len() as f64 * 1000.0 / total.max(1.0);
    let bucket = |lo: f64, hi: f64| ms.iter().filter(|&&m| m >= lo && m < hi).count();
    let buckets = format!(
        "<14:{} 14-19:{} 19-30:{} 30-37:{} >37:{}",
        bucket(0.0, 14.0),
        bucket(14.0, 19.0),
        bucket(19.0, 30.0),
        bucket(30.0, 37.0),
        bucket(37.0, f64::MAX)
    )
    .replace(' ', ",");
    ms.sort_by(f64::total_cmp);
    let pct = |q: f64| ms.get(((ms.len() as f64 - 1.0) * q) as usize).copied().unwrap_or(0.0);
    format!(
        "{} setup={}ms fps={fps:.2} ms={:.2}/{:.2}/{:.2} hist={buckets}",
        presented.len(),
        setup.as_millis(),
        pct(0.5),
        pct(0.99),
        pct(1.0)
    )
}

/// # Safety
/// `o`'s handles are `d`'s (or null), made through these loaders.
unsafe fn teardown(
    d: &ash::Device,
    surface_i: &ash::khr::surface::Instance,
    swap_d: &ash::khr::swapchain::Device,
    o: &mut Objects,
) {
    // SAFETY: the caller's guarantee; idle first, so nothing queued still uses them.
    unsafe {
        let _ = d.device_wait_idle();
        for s in o.ready.drain(..).chain(o.done.drain(..)) {
            d.destroy_semaphore(s, None);
        }
        for f in o.fences.drain(..) {
            d.destroy_fence(f, None);
        }
        d.destroy_command_pool(o.commands, None);
        d.destroy_descriptor_pool(o.descriptors, None);
        d.destroy_pipeline(o.pipeline, None);
        d.destroy_pipeline_layout(o.layout, None);
        d.destroy_descriptor_set_layout(o.set_layout, None);
        d.destroy_sampler(o.sampler, None);
        d.destroy_image_view(o.texture_view, None);
        d.destroy_image(o.texture, None);
        d.free_memory(o.texture_mem, None);
        d.destroy_buffer(o.staging, None);
        d.free_memory(o.staging_mem, None);
        for f in o.framebuffers.drain(..) {
            d.destroy_framebuffer(f, None);
        }
        d.destroy_render_pass(o.pass, None);
        for v in o.views.drain(..) {
            d.destroy_image_view(v, None);
        }
        swap_d.destroy_swapchain(o.swapchain, None);
        surface_i.destroy_surface(o.surface, None);
    }
}
