//! Firmware version and hardware model. Externs from `services/set.h`, libnx 4.12.0. Each call
//! opens set:sys and closes it again; libnx counts the opens.

/// `SetSysFirmwareVersion`, 0x100 bytes of `u8`/`char`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SetSysFirmwareVersion {
    pub major: u8,
    pub minor: u8,
    pub micro: u8,
    pub padding1: u8,
    pub revision_major: u8,
    pub revision_minor: u8,
    pub padding2: u8,
    pub padding3: u8,
    pub platform: [u8; 0x20],
    pub version_hash: [u8; 0x40],
    pub display_version: [u8; 0x18],
    pub display_title: [u8; 0x80],
}

/// A NUL-terminated C string in a fixed array, as UTF-8.
pub fn c_string(bytes: &[u8]) -> Option<String> {
    let s = core::ffi::CStr::from_bytes_until_nul(bytes).ok()?.to_str().ok()?;
    (!s.is_empty()).then(|| s.to_owned())
}

/// `SetSysProductModel` → the board's name.
pub fn model_name(model: i32) -> Option<&'static str> {
    Some(match model {
        1 => "Erista",
        2 => "Erista (Copper)",
        3 => "Mariko",
        4 => "Mariko Lite",
        5 => "Mariko (Calcio)",
        6 => "Mariko OLED",
        _ => return None,
    })
}

#[cfg(target_os = "horizon")]
mod ffi {
    extern "C" {
        pub fn setsysInitialize() -> u32;
        pub fn setsysExit();
        pub fn setsysGetFirmwareVersion(out: *mut super::SetSysFirmwareVersion) -> u32;
        /// `model` is a `SetSysProductModel`.
        pub fn setsysGetProductModel(model: *mut i32) -> u32;
    }
}

/// Runs `f` inside one set:sys session.
#[cfg(target_os = "horizon")]
fn with_setsys<T>(f: impl FnOnce() -> Option<T>) -> Option<T> {
    // SAFETY: no arguments; balanced by setsysExit.
    if unsafe { ffi::setsysInitialize() } != 0 {
        return None;
    }
    let out = f();
    // SAFETY: balances the setsysInitialize above.
    unsafe { ffi::setsysExit() };
    out
}

/// The firmware's display version, such as `"19.0.1"`.
#[cfg(target_os = "horizon")]
pub fn firmware_version() -> Option<String> {
    with_setsys(|| {
        // SAFETY: an all-byte struct; all-zero is valid.
        let mut v: SetSysFirmwareVersion = unsafe { core::mem::zeroed() };
        // SAFETY: set:sys is open; `v` is the 0x100-byte struct the call fills.
        if unsafe { ffi::setsysGetFirmwareVersion(&mut v) } != 0 {
            return None;
        }
        c_string(&v.display_version)
    })
}

/// The board: `"Erista"`, `"Mariko"`, `"Mariko Lite"`, `"Mariko OLED"`…
#[cfg(target_os = "horizon")]
pub fn product_model() -> Option<&'static str> {
    with_setsys(|| {
        let mut model = 0i32;
        // SAFETY: set:sys is open; `model` is a valid out-pointer.
        if unsafe { ffi::setsysGetProductModel(&mut model) } != 0 {
            return None;
        }
        model_name(model)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_and_models() {
        let mut v = [0u8; 0x18];
        v[..6].copy_from_slice(b"19.0.1");
        assert_eq!(c_string(&v).as_deref(), Some("19.0.1"));
        assert_eq!(c_string(&[0; 4]), None);
        assert_eq!(c_string(b"no nul"), None);
        assert_eq!(model_name(1), Some("Erista"));
        assert_eq!(model_name(6), Some("Mariko OLED"));
        assert_eq!(model_name(0), None);
    }
}
