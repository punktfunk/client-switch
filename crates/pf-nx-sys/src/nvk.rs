//! The statically linked NVK's one entry point. There is no Vulkan loader on the console: every
//! other command resolves through `vkGetInstanceProcAddr`, which `libvulkan_bh.a` keeps global
//! under its own name (bevy-horizon `scripts/localize_nvk.sh`, the `^vk[A-Z]` symbols).

use ash::vk;
use core::ffi::c_char;

extern "system" {
    /// Vulkan 1.0's `vkGetInstanceProcAddr`, defined by NVK's `vulkan-dispatch.o`.
    fn vkGetInstanceProcAddr(instance: vk::Instance, p_name: *const c_char) -> vk::PFN_vkVoidFunction;
}

/// NVK as an `ash::Entry`.
pub fn entry() -> ash::Entry {
    // SAFETY: the linked driver's own vkGetInstanceProcAddr, valid for the whole process,
    // resolves every global and instance command as a loader would.
    unsafe {
        ash::Entry::from_static_fn(ash::StaticFn {
            get_instance_proc_addr: vkGetInstanceProcAddr,
        })
    }
}
