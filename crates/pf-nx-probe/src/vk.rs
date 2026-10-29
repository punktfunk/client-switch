//! Check 4, `vulkan`: the linked NVK's instance and first device, then a 16 MiB host-visible
//! buffer written through one mapping and read back through a second, then the swapchain half
//! (`crate::swap`) on the same device. Steps stop at the first failure, and everything made is
//! destroyed in reverse order.

use std::ffi::{c_char, CStr};
use std::time::Instant;

use ash::vk;

use crate::{swap, Out, Steps};

const BUF_BYTES: vk::DeviceSize = 16 << 20;

/// A failed step and why.
pub(crate) type Fail = (&'static str, String);

pub fn steps(out: &mut Out) -> Steps {
    let mut steps = Steps::new();
    if let Err((step, why)) = instance_steps(&mut steps, out) {
        steps.push((step, Err(why)));
    }
    steps
}

pub(crate) fn at(step: &'static str) -> impl Fn(vk::Result) -> Fail {
    move |e| (step, format!("{e:?}"))
}

fn version(v: u32) -> String {
    let (major, minor, patch) = (
        vk::api_version_major(v),
        vk::api_version_minor(v),
        vk::api_version_patch(v),
    );
    format!("{major}.{minor}.{patch}")
}

fn join<T: ToString>(items: impl Iterator<Item = T>) -> String {
    items.map(|i| i.to_string()).collect::<Vec<_>>().join(",")
}

fn instance_steps(steps: &mut Steps, out: &mut Out) -> Result<(), Fail> {
    let entry = pf_nx_sys::nvk::entry();
    // SAFETY: global commands of the linked driver; they take no handles.
    let loader = unsafe { entry.try_enumerate_instance_version() }.map_err(at("loader"))?;
    // SAFETY: as above.
    let exts = unsafe { entry.enumerate_instance_extension_properties(None) }.map_err(at("ext"))?;
    // SAFETY: as above.
    let layers = unsafe { entry.enumerate_instance_layer_properties() }.map_err(at("layers"))?;
    let offered: Vec<&CStr> = exts.iter().filter_map(|e| e.extension_name_as_c_str().ok()).collect();
    steps.push(("loader", Ok(version(loader.unwrap_or(vk::API_VERSION_1_0)))));
    steps.push(("ext", Ok(join(offered.iter().map(|n| n.to_string_lossy())))));
    steps.push(("layers", Ok(layers.len().to_string())));

    // VK_NN_vi_surface needs VK_KHR_surface; each goes in when the driver offers it.
    let wanted: Vec<*const c_char> = [ash::khr::surface::NAME, ash::nn::vi_surface::NAME]
        .into_iter()
        .filter(|w| offered.contains(w))
        .map(CStr::as_ptr)
        .collect();
    let app = vk::ApplicationInfo::default()
        .application_name(c"pf-nx-probe")
        .api_version(vk::API_VERSION_1_3);
    let info = vk::InstanceCreateInfo::default()
        .application_info(&app)
        .enabled_extension_names(&wanted);
    // SAFETY: `info` and the names it points at outlive the call.
    let instance = unsafe { entry.create_instance(&info, None) }.map_err(at("instance"))?;
    steps.push(("instance", Ok(format!("1.3+{}ext", wanted.len()))));
    let result = device_steps(&entry, &instance, steps, out);
    // SAFETY: device_steps destroyed every child of the instance.
    unsafe { instance.destroy_instance(None) };
    result?;
    steps.push(("teardown", Ok("ok".into())));
    Ok(())
}

fn device_steps(entry: &ash::Entry, instance: &ash::Instance, steps: &mut Steps, out: &mut Out) -> Result<(), Fail> {
    // SAFETY: a live instance.
    let gpus = unsafe { instance.enumerate_physical_devices() }.map_err(at("gpus"))?;
    let gpu = *gpus.first().ok_or(("gpus", "0".to_string()))?;
    // SAFETY: `gpu` came from this instance.
    let (props, families, memory) = unsafe {
        (
            instance.get_physical_device_properties(gpu),
            instance.get_physical_device_queue_family_properties(gpu),
            instance.get_physical_device_memory_properties(gpu),
        )
    };
    let name = props
        .device_name_as_c_str()
        .map_or_else(|_| "?".into(), |n| n.to_string_lossy().replace(' ', "_"));
    let queue_flags = |f: vk::QueueFlags| -> String {
        [
            (vk::QueueFlags::GRAPHICS, 'G'),
            (vk::QueueFlags::COMPUTE, 'C'),
            (vk::QueueFlags::TRANSFER, 'T'),
            (vk::QueueFlags::SPARSE_BINDING, 'S'),
        ]
        .into_iter()
        .filter_map(|(bit, c)| f.contains(bit).then_some(c))
        .collect()
    };
    let local = |h: &vk::MemoryHeap| match h.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL) {
        true => "L",
        false => "",
    };
    steps.extend([
        ("gpus", Ok(gpus.len().to_string())),
        ("name", Ok(name)),
        ("api", Ok(version(props.api_version))),
        (
            "driver",
            Ok(format!(
                "{}({:#x})",
                version(props.driver_version),
                props.driver_version
            )),
        ),
        ("id", Ok(format!("{:04x}:{:04x}", props.vendor_id, props.device_id))),
        (
            "queues",
            Ok(join(families.iter().enumerate().map(|(i, f)| {
                format!("{i}:{}x{}", queue_flags(f.queue_flags), f.queue_count)
            }))),
        ),
        (
            "heaps",
            Ok(join(
                memory
                    .memory_heaps_as_slice()
                    .iter()
                    .map(|h| format!("{}MiB{}", h.size >> 20, local(h))),
            )),
        ),
    ]);

    let family = families
        .iter()
        .position(|f| f.queue_flags.contains(vk::QueueFlags::GRAPHICS))
        .ok_or(("device", "no graphics family".to_string()))? as u32;
    let queues = [vk::DeviceQueueCreateInfo::default()
        .queue_family_index(family)
        .queue_priorities(&[1.0])];
    // SAFETY: `gpu` came from this instance.
    let offered = unsafe { instance.enumerate_device_extension_properties(gpu) }.map_err(at("device"))?;
    let swapchain = offered
        .iter()
        .any(|e| e.extension_name_as_c_str().is_ok_and(|n| n == ash::khr::swapchain::NAME));
    let exts: Vec<*const c_char> = if swapchain {
        vec![ash::khr::swapchain::NAME.as_ptr()]
    } else {
        Vec::new()
    };
    let info = vk::DeviceCreateInfo::default()
        .queue_create_infos(&queues)
        .enabled_extension_names(&exts);
    // SAFETY: `gpu` came from this instance; `info` and the names outlive the call.
    let device = unsafe { instance.create_device(gpu, &info, None) }.map_err(at("device"))?;
    // SAFETY: the device was made with one queue in `family`.
    let queue = unsafe { device.get_device_queue(family, 0) };
    steps.push(match queue == vk::Queue::null() {
        true => ("device", Err(format!("family{family} has no queue 0"))),
        false => ("device", Ok(format!("family{family}"))),
    });
    let mut result = buffer_steps(&device, &memory, steps);
    if result.is_ok() {
        if swapchain {
            let gpu = swap::Gpu {
                entry,
                instance,
                pdev: gpu,
                device: &device,
                family,
                queue,
            };
            swap::steps(&gpu, steps, out);
        } else {
            result = Err(("swapchain", "no VK_KHR_swapchain".into()));
        }
    }
    // SAFETY: buffer_steps and swap::steps destroyed the device's children and left it idle.
    unsafe { device.destroy_device(None) };
    result
}

fn buffer_steps(
    device: &ash::Device,
    memory: &vk::PhysicalDeviceMemoryProperties,
    steps: &mut Steps,
) -> Result<(), Fail> {
    let info = vk::BufferCreateInfo::default()
        .size(BUF_BYTES)
        .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST);
    // SAFETY: a live device; `info` outlives the call.
    let buffer = unsafe { device.create_buffer(&info, None) }.map_err(at("buffer"))?;
    let result = memory_steps(device, buffer, memory, steps);
    // SAFETY: the buffer was never used by the GPU.
    unsafe { device.destroy_buffer(buffer, None) };
    result
}

fn memory_steps(
    device: &ash::Device,
    buffer: vk::Buffer,
    memory: &vk::PhysicalDeviceMemoryProperties,
    steps: &mut Steps,
) -> Result<(), Fail> {
    // SAFETY: `buffer` belongs to `device`.
    let needs = unsafe { device.get_buffer_memory_requirements(buffer) };
    let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
    let kind = memory
        .memory_types_as_slice()
        .iter()
        .enumerate()
        .position(|(i, t)| needs.memory_type_bits & (1 << i) != 0 && t.property_flags.contains(host))
        .ok_or(("memory", "no host-visible coherent type".to_string()))? as u32;
    let info = vk::MemoryAllocateInfo::default()
        .allocation_size(needs.size)
        .memory_type_index(kind);
    // SAFETY: a live device; `info` outlives the call.
    let mem = unsafe { device.allocate_memory(&info, None) }.map_err(at("memory"))?;
    steps.push(("memory", Ok(format!("{}MiB@type{kind}", needs.size >> 20))));
    let result = round_trip(device, buffer, mem, steps);
    // SAFETY: round_trip unmapped it; the buffer bound to it is never used again.
    unsafe { device.free_memory(mem, None) };
    result
}

/// Writes a pattern through one mapping and checks it through a second.
fn round_trip(device: &ash::Device, buffer: vk::Buffer, mem: vk::DeviceMemory, steps: &mut Steps) -> Result<(), Fail> {
    let words = (BUF_BYTES / 4) as usize;
    let pattern = |i: usize| (i as u32).wrapping_mul(0x9E37_79B9);
    // SAFETY: unbound memory of the buffer's required size and type, at offset 0.
    unsafe { device.bind_buffer_memory(buffer, mem, 0) }.map_err(at("bind"))?;
    let map = |step| {
        // SAFETY: `mem` is host-visible and not mapped; the mapping covers BUF_BYTES or more.
        unsafe { device.map_memory(mem, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty()) }.map_err(at(step))
    };

    let ptr = map("write")?.cast::<u32>();
    let start = Instant::now();
    for i in 0..words {
        // SAFETY: `ptr` maps at least `words` u32s, aligned to the memory's alignment.
        unsafe { ptr.add(i).write(pattern(i)) };
    }
    let wrote = start.elapsed();
    // SAFETY: mapped above.
    unsafe { device.unmap_memory(mem) };
    steps.push(("write", Ok(format!("{}us", wrote.as_micros()))));

    let ptr = map("read")?.cast::<u32>();
    let start = Instant::now();
    // SAFETY: as for the write.
    let wrong = (0..words)
        .filter(|&i| unsafe { ptr.add(i).read() } != pattern(i))
        .count();
    let read = start.elapsed();
    // SAFETY: mapped above.
    unsafe { device.unmap_memory(mem) };
    if wrong != 0 {
        return Err(("read", format!("{wrong}/{words}wrong")));
    }
    steps.push(("read", Ok(format!("{}us", read.as_micros()))));
    Ok(())
}
