//! The CPU, GPU and memory clocks, read-only. Externs from `services/clkrst.h` ([8.0.0+]) and
//! `services/pcv.h` (the module ids), libnx 4.12.0. Firmware before 8.0.0 reads `None`.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Clocks {
    pub cpu_hz: u32,
    pub gpu_hz: u32,
    pub emc_hz: u32,
}

/// `PcvModuleId_CpuBus`, `PcvModuleId_GPU`, `PcvModuleId_EMC`.
pub const MODULE_CPU: u32 = 0x4000_0001;
pub const MODULE_GPU: u32 = 0x4000_0002;
pub const MODULE_EMC: u32 = 0x4000_0039;

/// `ClkrstSession`: one `Service`, 16 bytes aligned to 4. Only libnx reads it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ClkrstSession(pub [u32; 4]);

#[cfg(target_os = "horizon")]
mod ffi {
    use super::ClkrstSession;

    extern "C" {
        pub fn clkrstInitialize() -> u32;
        pub fn clkrstExit();
        /// `module_id` is a `PcvModuleId`; official sysmodules pass `unk = 3`.
        pub fn clkrstOpenSession(session_out: *mut ClkrstSession, module_id: u32, unk: u32) -> u32;
        pub fn clkrstCloseSession(session: *mut ClkrstSession);
        pub fn clkrstGetClockRate(session: *mut ClkrstSession, out_hz: *mut u32) -> u32;
    }
}

#[cfg(target_os = "horizon")]
fn rate(module: u32) -> Option<u32> {
    let mut s = ClkrstSession::default();
    // SAFETY: clkrst is open (see `clocks`); `s` receives the session.
    if unsafe { ffi::clkrstOpenSession(&mut s, module, 3) } != 0 {
        return None;
    }
    let mut hz = 0u32;
    // SAFETY: `s` is the session just opened; closed right after.
    let rc = unsafe { ffi::clkrstGetClockRate(&mut s, &mut hz) };
    // SAFETY: closes the session opened above.
    unsafe { ffi::clkrstCloseSession(&mut s) };
    (rc == 0).then_some(hz)
}

/// The three clocks now, or `None` if clkrst is unavailable.
#[cfg(target_os = "horizon")]
pub fn clocks() -> Option<Clocks> {
    // SAFETY: no arguments; balanced by clkrstExit.
    if unsafe { ffi::clkrstInitialize() } != 0 {
        return None;
    }
    let c = (|| {
        Some(Clocks {
            cpu_hz: rate(MODULE_CPU)?,
            gpu_hz: rate(MODULE_GPU)?,
            emc_hz: rate(MODULE_EMC)?,
        })
    })();
    // SAFETY: balances the clkrstInitialize above.
    unsafe { ffi::clkrstExit() };
    c
}
