//! Small allocation helpers over the one device.

use anyhow::{anyhow, Result};
use ash::vk;

use crate::device::GpuDevice;

/// A 2D image with its own memory. The caller destroys both.
pub fn image(
    dev: &GpuDevice,
    extent: vk::Extent2D,
    format: vk::Format,
    usage: vk::ImageUsageFlags,
    props: vk::MemoryPropertyFlags,
) -> Result<(vk::Image, vk::DeviceMemory)> {
    let ci = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(format)
        .extent(vk::Extent3D {
            width: extent.width,
            height: extent.height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(usage)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    // SAFETY: `ci` outlives the call; the image is returned to the caller, who destroys it.
    let image = unsafe { dev.device.create_image(&ci, None) }?;
    // SAFETY: the image is live.
    let req = unsafe { dev.device.get_image_memory_requirements(image) };
    let memory = allocate(dev, req, props).inspect_err(|_| {
        // SAFETY: nothing is bound to the image yet.
        unsafe { dev.device.destroy_image(image, None) };
    })?;
    // SAFETY: fresh image and memory of the required size and type.
    unsafe { dev.device.bind_image_memory(image, memory, 0) }?;
    Ok((image, memory))
}

/// A buffer with host-visible, coherent memory, mapped for its life. The caller destroys both.
pub fn host_buffer(
    dev: &GpuDevice,
    size: u64,
    usage: vk::BufferUsageFlags,
) -> Result<(vk::Buffer, vk::DeviceMemory, *mut u8)> {
    let ci = vk::BufferCreateInfo::default()
        .size(size)
        .usage(usage)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    // SAFETY: `ci` outlives the call.
    let buffer = unsafe { dev.device.create_buffer(&ci, None) }?;
    // SAFETY: the buffer is live.
    let req = unsafe { dev.device.get_buffer_memory_requirements(buffer) };
    let memory = allocate(
        dev,
        req,
        vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
    )
    .inspect_err(|_| {
        // SAFETY: nothing is bound yet.
        unsafe { dev.device.destroy_buffer(buffer, None) };
    })?;
    // SAFETY: fresh buffer and memory of the required size; the whole range is mapped once and
    // stays mapped until the memory is freed.
    let ptr = unsafe {
        dev.device.bind_buffer_memory(buffer, memory, 0)?;
        dev.device
            .map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())?
    };
    Ok((buffer, memory, ptr.cast()))
}

fn allocate(dev: &GpuDevice, req: vk::MemoryRequirements, props: vk::MemoryPropertyFlags) -> Result<vk::DeviceMemory> {
    let index = dev
        .memory_type(req.memory_type_bits, props)
        .ok_or_else(|| anyhow!("no memory type with {props:?}"))?;
    // SAFETY: the allocate-info is a local; the memory is returned to the caller.
    Ok(unsafe {
        dev.device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(req.size)
                .memory_type_index(index),
            None,
        )
    }?)
}

/// A colour view of a whole 2D image.
pub fn view(dev: &GpuDevice, image: vk::Image, format: vk::Format) -> Result<vk::ImageView> {
    // SAFETY: the builder is a local; the view is returned to the caller, who destroys it.
    Ok(unsafe {
        dev.device.create_image_view(
            &vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(format)
                .subresource_range(color_range()),
            None,
        )
    }?)
}

pub fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        level_count: 1,
        layer_count: 1,
        ..Default::default()
    }
}

/// One image layout transition, recorded into `cmd`.
///
/// # Safety
/// `cmd` is recording on `dev`'s device and `image` is live.
pub unsafe fn barrier(
    dev: &GpuDevice,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    from: vk::ImageLayout,
    to: vk::ImageLayout,
    src: (vk::PipelineStageFlags2, vk::AccessFlags2),
    dst: (vk::PipelineStageFlags2, vk::AccessFlags2),
) {
    let barrier = [vk::ImageMemoryBarrier2::default()
        .src_stage_mask(src.0)
        .src_access_mask(src.1)
        .dst_stage_mask(dst.0)
        .dst_access_mask(dst.1)
        .old_layout(from)
        .new_layout(to)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(color_range())];
    // SAFETY: the caller guarantees `cmd` is recording and `image` is live.
    unsafe {
        dev.device
            .cmd_pipeline_barrier2(cmd, &vk::DependencyInfo::default().image_memory_barriers(&barrier));
    }
}

/// Linear, clamped. Chroma is upsampled by the sampler and the UI layer is scaled by it.
pub fn sampler(dev: &GpuDevice) -> Result<vk::Sampler> {
    // SAFETY: the builder is a local; the sampler is returned to the caller.
    Ok(unsafe {
        dev.device.create_sampler(
            &vk::SamplerCreateInfo::default()
                .mag_filter(vk::Filter::LINEAR)
                .min_filter(vk::Filter::LINEAR)
                .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE),
            None,
        )
    }?)
}
