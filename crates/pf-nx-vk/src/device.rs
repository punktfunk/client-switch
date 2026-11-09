//! The one Vulkan device: instance, physical device, logical device and queue, exported as the
//! [`VulkanDecodeDevice`] the shared `PyroWave` decoder builds on.
//!
//! NVK is not conformant on the X1, so the device asks for the least the presenter needs:
//! `synchronization2` for its barriers, and `VK_EXT_external_memory_host` where offered, for
//! pictures sampled in place. Robust buffer access stays off: on the
//! X1's SM 5.3 NVK checks each storage access with a branch. The `PyroWave` conjuncts (int16,
//! 8-bit storage, subgroup control, timeline semaphores) are asked for only with the
//! `pyrowave` feature and only where the probe found them all.

use std::ffi::{c_void, CStr, CString};

use anyhow::{anyhow, Context, Result};
use ash::vk::{self, Handle as _};
use pf_client_video::video_vk::{QueueLock, VulkanDecodeDevice};

/// The Vulkan version the instance declares. Skia validates its function table against the
/// version it is handed, and a device's own newer one fails that check.
pub const INSTANCE_API_VERSION: u32 = vk::API_VERSION_1_3;

/// The display's vblank, as the platform hands it over.
#[derive(Clone, Copy, Debug)]
pub struct Vsync {
    /// Drops a vblank already signalled, so the next `wait` is for one after the call.
    pub clear: fn(),
    /// Blocks until a vblank, at most the nanoseconds given; `false` on a timeout.
    pub wait: fn(u64) -> bool,
}

/// What the device presents to.
#[derive(Clone, Copy, Debug)]
pub enum Target {
    /// `VK_NN_vi_surface` on a libnx `NWindow`. `size` is the swapchain extent asked for; the
    /// surface takes the window's size, so `resize` sizes the window before each surface is made.
    /// `vsync` is the display's vblank, where vi gave one; `queued` says whether the last present
    /// found an earlier frame still waiting for the panel.
    Vi {
        window: *mut c_void,
        size: (u32, u32),
        resize: fn(*mut c_void, u32, u32) -> bool,
        vsync: Option<Vsync>,
        queued: fn() -> bool,
    },
    /// Images this crate owns, read back for tests. A developer box without a window.
    Offscreen { width: u32, height: u32 },
}

// SAFETY: the window is libnx's default `NWindow`, which lives for the whole process; only the
// presenter hands it to the driver, once per surface.
unsafe impl Send for Target {}
// SAFETY: as above; nothing reads through the pointer on this side.
unsafe impl Sync for Target {}

/// The device every part of the title draws and decodes on. Dropping idles and destroys it.
pub struct GpuDevice {
    pub entry: ash::Entry,
    pub instance: ash::Instance,
    pub pdev: vk::PhysicalDevice,
    pub device: ash::Device,
    pub queue: vk::Queue,
    pub qf: u32,
    pub api_version: u32,
    pub name: String,
    /// The handoff the `PyroWave` decoder is built from; carries the shared queue lock.
    pub vkd: VulkanDecodeDevice,
    /// `VK_EXT_external_memory_host` is on: host memory can be imported.
    pub host_import: bool,
    pub target: Target,
    messenger: Option<Messenger>,
}

/// The driver's own error text. A release Mesa sends it only to a debug messenger, and the
/// console has no other place it would show.
struct Messenger {
    ext: ash::ext::debug_utils::Instance,
    handle: vk::DebugUtilsMessengerEXT,
}

struct Probe {
    ok: bool,
    float16: bool,
    api_version: u32,
    vendor_id: u32,
    name: String,
}

fn probe(instance: &ash::Instance, pdev: vk::PhysicalDevice) -> Probe {
    // SAFETY: read-only query on a live physical device; the result is plain data.
    let props = unsafe { instance.get_physical_device_properties(pdev) };
    let name = props
        .device_name_as_c_str()
        .map(|c| c.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut f12 = vk::PhysicalDeviceVulkan12Features::default();
    let mut f13 = vk::PhysicalDeviceVulkan13Features::default();
    let mut f2 = vk::PhysicalDeviceFeatures2::default()
        .push_next(&mut f12)
        .push_next(&mut f13);
    // SAFETY: read-only query; the chained locals outlive the call.
    unsafe { instance.get_physical_device_features2(pdev, &mut f2) };
    let int16 = f2.features.shader_int16;
    Probe {
        ok: props.api_version >= vk::API_VERSION_1_3
            && int16 == vk::TRUE
            && f12.storage_buffer8_bit_access == vk::TRUE
            && f12.timeline_semaphore == vk::TRUE
            && f13.subgroup_size_control == vk::TRUE
            && f13.compute_full_subgroups == vk::TRUE
            && f13.synchronization2 == vk::TRUE,
        float16: f12.shader_float16 == vk::TRUE,
        api_version: props.api_version,
        vendor_id: props.vendor_id,
        name,
    }
}

unsafe extern "system" fn driver_message(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _types: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    _user: *mut c_void,
) -> vk::Bool32 {
    // SAFETY: the driver passes a live callback-data struct whose message is NUL-terminated for
    // the length of the call; nothing is kept past it.
    let text = unsafe { data.as_ref().and_then(|d| d.message_as_c_str()) }
        .map(CStr::to_string_lossy)
        .unwrap_or_default();
    if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
        tracing::error!("vulkan: {text}");
    } else {
        tracing::warn!("vulkan: {text}");
    }
    vk::FALSE
}

impl GpuDevice {
    /// Brings the device up for `target`. `entry` is the statically linked NVK on the console
    /// (`pf_nx_sys::nvk::entry`), the system loader elsewhere.
    pub fn new(entry: ash::Entry, target: Target) -> Result<Self> {
        // SAFETY: a global query with no layer name.
        let offered = unsafe { entry.enumerate_instance_extension_properties(None) }.unwrap_or_default();
        let debug_utils = offered
            .iter()
            .any(|e| e.extension_name_as_c_str() == Ok(ash::ext::debug_utils::NAME));
        let mut instance_exts: Vec<&CStr> = match target {
            Target::Vi { .. } => vec![ash::khr::surface::NAME, ash::nn::vi_surface::NAME],
            Target::Offscreen { .. } => Vec::new(),
        };
        if debug_utils {
            instance_exts.push(ash::ext::debug_utils::NAME);
        }
        let app = vk::ApplicationInfo::default()
            .application_name(c"punktfunk-switch")
            .api_version(INSTANCE_API_VERSION);
        let ptrs: Vec<*const std::ffi::c_char> = instance_exts.iter().map(|n| n.as_ptr()).collect();
        // SAFETY: the create-info and the names it borrows outlive the call; the instance is
        // owned by the returned value and destroyed in `Drop`.
        let instance = unsafe {
            entry.create_instance(
                &vk::InstanceCreateInfo::default()
                    .application_info(&app)
                    .enabled_extension_names(&ptrs),
                None,
            )
        }
        .map_err(|e| anyhow!("vkCreateInstance: {e}"))?;
        let messenger = debug_utils.then(|| messenger(&entry, &instance)).flatten();
        match Self::with_instance(entry, instance.clone(), &instance_exts, target) {
            Ok(mut dev) => {
                dev.messenger = messenger;
                Ok(dev)
            }
            Err(e) => {
                // SAFETY: nothing else was created from the instance that outlives this point.
                unsafe {
                    if let Some(m) = messenger {
                        m.ext.destroy_debug_utils_messenger(m.handle, None);
                    }
                    instance.destroy_instance(None);
                }
                Err(e)
            }
        }
    }

    fn with_instance(
        entry: ash::Entry,
        instance: ash::Instance,
        instance_exts: &[&CStr],
        target: Target,
    ) -> Result<Self> {
        // SAFETY: the instance is live; the enumeration only reads.
        let devices = unsafe { instance.enumerate_physical_devices() }.context("enumerate GPUs")?;
        let (pdev, qf, p) = devices
            .into_iter()
            .find_map(|pdev| {
                let p = probe(&instance, pdev);
                tracing::info!(
                    "gpu: {} Vulkan {}.{} pyrowave-capable={}",
                    p.name,
                    vk::api_version_major(p.api_version),
                    vk::api_version_minor(p.api_version),
                    p.ok
                );
                // SAFETY: read-only query on a live physical device.
                let families = unsafe { instance.get_physical_device_queue_family_properties(pdev) };
                let qf = families.iter().position(|f| {
                    f.queue_flags
                        .contains(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE)
                })?;
                // TODO(verify): the API version NVK reports on the X1 (bevy-horizon's nvk_probe logs it).
                (p.api_version >= vk::API_VERSION_1_3).then_some((pdev, qf as u32, p))
            })
            .ok_or_else(|| anyhow!("no Vulkan 1.3 device with a graphics+compute queue"))?;

        let pyro = cfg!(feature = "pyrowave") && p.ok;
        let mut device_exts: Vec<&CStr> = match target {
            Target::Vi { .. } => vec![ash::khr::swapchain::NAME],
            Target::Offscreen { .. } => Vec::new(),
        };
        // SAFETY: read-only query on a live physical device.
        let offered = unsafe { instance.enumerate_device_extension_properties(pdev) }.unwrap_or_default();
        let host_import = offered
            .iter()
            .any(|e| e.extension_name_as_c_str() == Ok(ash::ext::external_memory_host::NAME));
        if host_import {
            device_exts.push(ash::ext::external_memory_host::NAME);
        }
        let ext_ptrs: Vec<*const std::ffi::c_char> = device_exts.iter().map(|n| n.as_ptr()).collect();
        let prio = [1.0f32];
        let queue_ci = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(qf)
            .queue_priorities(&prio)];
        // float16 only where the driver has it: pyrowave picks a half-precision path off this report.
        let mut f12 = vk::PhysicalDeviceVulkan12Features::default()
            .timeline_semaphore(pyro)
            .storage_buffer8_bit_access(pyro)
            .shader_float16(pyro && p.float16);
        let mut f13 = vk::PhysicalDeviceVulkan13Features::default()
            .synchronization2(true)
            .subgroup_size_control(pyro)
            .compute_full_subgroups(pyro);
        let mut f2 = vk::PhysicalDeviceFeatures2::default()
            .push_next(&mut f12)
            .push_next(&mut f13);
        f2.features.shader_int16 = if pyro { vk::TRUE } else { vk::FALSE };
        // SAFETY: every builder is a local outliving the call; the device is owned by the
        // returned value and destroyed in `Drop`.
        let device = unsafe {
            instance.create_device(
                pdev,
                &vk::DeviceCreateInfo::default()
                    .queue_create_infos(&queue_ci)
                    .enabled_extension_names(&ext_ptrs)
                    .push_next(&mut f2),
                None,
            )
        }
        .map_err(|e| anyhow!("vkCreateDevice: {e}"))?;
        // SAFETY: queue 0 of `qf` was requested at create.
        let queue = unsafe { device.get_device_queue(qf, 0) };
        tracing::info!("gpu: device up on {} (pyrowave {pyro})", p.name);
        let vkd = VulkanDecodeDevice {
            get_instance_proc_addr: entry.static_fn().get_instance_proc_addr as usize,
            instance: instance.handle().as_raw() as usize,
            physical_device: pdev.as_raw() as usize,
            device: device.handle().as_raw() as usize,
            vendor_id: p.vendor_id,
            device_name: p.name.clone(),
            graphics_qf: qf,
            // No Vulkan Video here; decode mirrors graphics so the fields pyrowave reads stay
            // well-formed.
            decode_qf: qf,
            decode_video_caps: 0,
            instance_extensions: instance_exts.iter().map(|n| CString::from(*n)).collect(),
            device_extensions: device_exts.iter().map(|n| CString::from(*n)).collect(),
            f_sampler_ycbcr: false,
            f_timeline_semaphore: pyro,
            f_synchronization2: true,
            video_decode: false,
            pyrowave_decode: pyro,
            f_shader_int16: pyro,
            f_storage_buffer8: pyro,
            f_subgroup_size_control: pyro,
            f_compute_full_subgroups: pyro,
            f_shader_float16: pyro && p.float16,
            api_version: p.api_version,
            queue_families: vec![qf],
            d3d11_import: false,
            dmabuf_import: false,
            vaapi_av1_decode: false,
            vaapi_hevc_decode: false,
            d3d11_hdr10: false,
            d3d11_nv12: false,
            d3d11_p010: false,
            d3d11_present: false,
            d3d11_fence: false,
            adapter_luid: None,
            queue_lock: std::sync::Arc::new(QueueLock::new()),
        };
        Ok(Self {
            entry,
            instance,
            pdev,
            device,
            queue,
            qf,
            api_version: p.api_version,
            name: p.name,
            vkd,
            host_import,
            target,
            messenger: None,
        })
    }

    /// The device's feature line for the probe and the first bundle.
    pub fn feature_line(&self) -> String {
        format!(
            "vulkan {}.{} {} pyrowave={} int16={} sb8={} subgroup_ctl={} full_subgroups={} f16={} host_import={} messenger={}",
            vk::api_version_major(self.api_version),
            vk::api_version_minor(self.api_version),
            self.name,
            self.vkd.pyrowave_decode,
            self.vkd.f_shader_int16,
            self.vkd.f_storage_buffer8,
            self.vkd.f_subgroup_size_control,
            self.vkd.f_compute_full_subgroups,
            self.vkd.f_shader_float16,
            self.host_import,
            self.messenger.is_some(),
        )
    }

    /// A memory type index with `props` among `bits`.
    pub fn memory_type(&self, bits: u32, props: vk::MemoryPropertyFlags) -> Option<u32> {
        // SAFETY: read-only query on a live physical device.
        let mem = unsafe { self.instance.get_physical_device_memory_properties(self.pdev) };
        (0..mem.memory_type_count)
            .find(|&i| bits & (1 << i) != 0 && mem.memory_types[i as usize].property_flags.contains(props))
    }
}

/// A messenger for the driver's errors and warnings, or `None` (logged) where it is refused.
fn messenger(entry: &ash::Entry, instance: &ash::Instance) -> Option<Messenger> {
    let ext = ash::ext::debug_utils::Instance::new(entry, instance);
    let ci = vk::DebugUtilsMessengerCreateInfoEXT::default()
        .message_severity(vk::DebugUtilsMessageSeverityFlagsEXT::WARNING | vk::DebugUtilsMessageSeverityFlagsEXT::ERROR)
        .message_type(
            vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
        )
        .pfn_user_callback(Some(driver_message));
    // SAFETY: `ci` outlives the call; the callback is a plain function that keeps nothing; the
    // messenger is destroyed before the instance (`Drop`, or `new`'s error path).
    match unsafe { ext.create_debug_utils_messenger(&ci, None) } {
        Ok(handle) => Some(Messenger { ext, handle }),
        Err(e) => {
            tracing::warn!("gpu: no debug messenger, driver errors arrive without their reason: {e}");
            None
        }
    }
}

impl Drop for GpuDevice {
    fn drop(&mut self) {
        let _q = self.vkd.queue_lock.guard();
        // SAFETY: every object built on this device belongs to a value that borrows or clones
        // it and is dropped first (the presenter, the decoders); idling retires queued work.
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_device(None);
            if let Some(m) = self.messenger.take() {
                m.ext.destroy_debug_utils_messenger(m.handle, None);
            }
            self.instance.destroy_instance(None);
        }
    }
}
