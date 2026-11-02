//! The system keyboard: libnx's `swkbd` library applet, through `csrc/pf_swkbd.c` (libnx 4.12.0
//! `applets/swkbd.h`), which keeps the applet's versioned config block on the C side.

/// The longest text one [`ask`] takes, in characters.
pub const MAX_CHARS: u32 = 256;

#[cfg(target_os = "horizon")]
mod ffi {
    use core::ffi::c_char;

    extern "C" {
        pub fn pf_swkbd_ask(
            guide: *const c_char,
            ok: *const c_char,
            max_chars: u32,
            out: *mut c_char,
            out_len: usize,
        ) -> u32;
    }
}

/// Shows the keyboard with `guide` in its empty field and `ok` on its confirm button, and blocks
/// until the player closes it. The committed text, or the applet's `Result` (a cancel included).
#[cfg(target_os = "horizon")]
pub fn ask(guide: &str, ok: &str) -> Result<String, u32> {
    let guide = std::ffi::CString::new(guide).unwrap_or_default();
    let ok = std::ffi::CString::new(ok).unwrap_or_default();
    // UTF-8 takes up to four bytes a character, and the shim NUL-terminates.
    let mut out = vec![0u8; MAX_CHARS as usize * 4 + 1];
    // SAFETY: both strings are NUL-terminated and outlive the call; `out` is writable for its
    // length, which the applet's reply is bounded by.
    let rc = unsafe {
        ffi::pf_swkbd_ask(
            guide.as_ptr(),
            ok.as_ptr(),
            MAX_CHARS,
            out.as_mut_ptr().cast(),
            out.len(),
        )
    };
    if rc != 0 {
        return Err(rc);
    }
    let end = out.iter().position(|&b| b == 0).unwrap_or(out.len());
    Ok(String::from_utf8_lossy(&out[..end]).into_owned())
}
