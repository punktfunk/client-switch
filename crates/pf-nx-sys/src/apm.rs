//! The performance configuration the system runs the title at while handheld: one row of its
//! fixed table, which a title asks for and the system grants. Externs from `services/apm.h`,
//! libnx 4.12.0.

/// The configuration for a handheld GPU level in MHz, with the CPU and the memory where the
/// default has them (1020 and 1331 MHz). `None` for a level the table has no such row for.
pub fn handheld_configuration(gpu_mhz: u32) -> Option<u32> {
    match gpu_mhz {
        384 => Some(0x0002_0004),
        460 => Some(0x9222_0008),
        _ => None,
    }
}

/// `ApmPerformanceMode_Normal`: handheld.
#[cfg(target_os = "horizon")]
const HANDHELD: i32 = 0;

#[cfg(target_os = "horizon")]
mod ffi {
    extern "C" {
        pub fn apmInitialize() -> u32;
        pub fn apmExit();
        pub fn apmSetPerformanceConfiguration(mode: i32, configuration: u32) -> u32;
        pub fn apmGetPerformanceConfiguration(mode: i32, configuration: *mut u32) -> u32;
    }
}

/// A handheld configuration the title asked the system for. It holds an apm session open, so
/// the answer lasts until [`Self::restore`].
#[cfg(target_os = "horizon")]
pub struct HandheldLevel {
    /// The configuration before the ask, when the system told it.
    before: Option<u32>,
}

#[cfg(target_os = "horizon")]
impl HandheldLevel {
    /// Asks for `configuration` while handheld. `Err` is libnx's result.
    pub fn ask(configuration: u32) -> Result<Self, u32> {
        // SAFETY: no arguments; balanced by the `apmExit` below or in `restore`.
        let rc = unsafe { ffi::apmInitialize() };
        if rc != 0 {
            return Err(rc);
        }
        let mut before = 0u32;
        // SAFETY: apm is open; `before` is a local that receives the configuration.
        let read = unsafe { ffi::apmGetPerformanceConfiguration(HANDHELD, &mut before) };
        // SAFETY: apm is open; both arguments are plain values.
        let rc = unsafe { ffi::apmSetPerformanceConfiguration(HANDHELD, configuration) };
        if rc != 0 {
            // SAFETY: balances the `apmInitialize` above.
            unsafe { ffi::apmExit() };
            return Err(rc);
        }
        Ok(Self {
            before: (read == 0).then_some(before),
        })
    }

    /// The configuration the ask replaced.
    pub fn before(&self) -> Option<u32> {
        self.before
    }

    /// Puts the former configuration back and closes the session.
    pub fn restore(&self) {
        if let Some(before) = self.before {
            // SAFETY: apm is open since `ask`; both arguments are plain values.
            unsafe { ffi::apmSetPerformanceConfiguration(HANDHELD, before) };
        }
        // SAFETY: balances the `apmInitialize` in `ask`.
        unsafe { ffi::apmExit() };
    }
}

#[cfg(test)]
mod tests {
    use super::handheld_configuration;

    /// Both rows keep the default's CPU and memory, so the A/B moves the GPU alone.
    #[test]
    fn a_gpu_level_names_its_handheld_row() {
        assert_eq!(handheld_configuration(384), Some(0x0002_0004));
        assert_eq!(handheld_configuration(460), Some(0x9222_0008));
        assert_eq!(handheld_configuration(307), None);
        assert_eq!(handheld_configuration(768), None);
    }
}
