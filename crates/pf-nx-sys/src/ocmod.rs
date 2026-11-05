//! The overclock module a player may run, through `csrc/pf_ocmod.c`. A module that sees the clock
//! change under it can put it back (Horizon OC does, with no profile set), so while one runs the
//! title asks it for the rate instead of setting clkrst itself.

#[cfg(target_os = "horizon")]
use core::ffi::{c_char, CStr};

/// `HocClkModule_CPU`, `SysClkModule_CPU`.
#[cfg(target_os = "horizon")]
const MODULE_CPU: u32 = 0;

/// Horizon OC's service, then sys-clk's and its forks'.
#[cfg(target_os = "horizon")]
const SERVICES: [&CStr; 2] = [c"hoc:clk", c"sys:clk"];

#[cfg(target_os = "horizon")]
mod ffi {
    use core::ffi::c_char;

    extern "C" {
        pub fn pf_ocmod_has(name: *const c_char, out: *mut u8) -> u32;
        pub fn pf_ocmod_info(name: *const c_char, api: *mut u32, version: *mut c_char, version_len: usize) -> u32;
        pub fn pf_ocmod_override(name: *const c_char, module: u32, hz: u32) -> u32;
    }
}

/// A running overclock module.
#[cfg(target_os = "horizon")]
#[derive(Clone, Copy, Debug)]
pub struct OcModule {
    service: &'static CStr,
}

#[cfg(target_os = "horizon")]
impl OcModule {
    /// The module that is running, if one is. `Err` is the libnx result when sm could not say.
    pub fn find() -> Result<Option<Self>, u32> {
        for service in SERVICES {
            let mut has = 0u8;
            // SAFETY: `service` is NUL-terminated and `has` is a writable byte.
            let rc = unsafe { ffi::pf_ocmod_has(service.as_ptr(), &mut has) };
            if rc != 0 {
                return Err(rc);
            }
            if has != 0 {
                return Ok(Some(Self { service }));
            }
        }
        Ok(None)
    }

    pub fn service(&self) -> &'static str {
        self.service.to_str().unwrap_or("?")
    }

    /// Its API version and version string.
    pub fn info(&self) -> Result<(u32, String), u32> {
        let mut api = 0u32;
        let mut version = [0 as c_char; 64];
        // SAFETY: `service` is NUL-terminated; `api` and `version` are writable for their sizes.
        let rc = unsafe { ffi::pf_ocmod_info(self.service.as_ptr(), &mut api, version.as_mut_ptr(), version.len()) };
        if rc != 0 {
            return Err(rc);
        }
        // The module may fill the buffer without a NUL.
        version[version.len() - 1] = 0;
        // SAFETY: NUL-terminated by the line above; `version` outlives the borrow.
        let text = unsafe { CStr::from_ptr(version.as_ptr()) };
        Ok((api, text.to_string_lossy().into_owned()))
    }

    /// Asks it to hold the CPU at `hz`; 0 hands the clock back. It lives in the module's memory
    /// until then, past the title's exit.
    pub fn hold_cpu(&self, hz: u32) -> Result<(), u32> {
        // SAFETY: `service` is NUL-terminated; the rest are plain values.
        match unsafe { ffi::pf_ocmod_override(self.service.as_ptr(), MODULE_CPU, hz) } {
            0 => Ok(()),
            rc => Err(rc),
        }
    }
}
