//! The system's shared fonts: the faces HOME draws Japanese, Chinese and Korean with. Externs
//! from `services/pl.h`, libnx 4.12.0. pl:u maps them read-only as plain TTF; `rt::userAppExit`
//! unmaps them as the process leaves.

/// `PlSharedFontType`: the text faces. `NintendoExt` (5) holds button glyphs, not text.
pub const STANDARD: u32 = 0;
pub const CHINESE_SIMPLIFIED: u32 = 1;
pub const EXT_CHINESE_SIMPLIFIED: u32 = 2;
pub const CHINESE_TRADITIONAL: u32 = 3;
pub const KO: u32 = 4;

/// `PlFontData`: where one face sits in the shared memory.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PlFontData {
    /// A `PlSharedFontType`.
    pub kind: u32,
    pub offset: u32,
    pub size: u32,
    pub address: *const u8,
}

/// The text faces for a BCP-47 `language`, its own first so a Han character takes its region's
/// shapes. A `zh` tag reads its script, else its region (TW, HK and MO write Traditional).
pub fn font_order(language: &str) -> Vec<u32> {
    let lower = language.to_ascii_lowercase();
    let mut subtags = lower.split('-');
    let lead: &[u32] = match (subtags.next(), subtags.next()) {
        (Some("ko"), _) => &[KO],
        (Some("zh"), Some("hant" | "tw" | "hk" | "mo")) => &[CHINESE_TRADITIONAL],
        (Some("zh"), _) => &[CHINESE_SIMPLIFIED, EXT_CHINESE_SIMPLIFIED],
        _ => &[],
    };
    let rest = (STANDARD..=KO).filter(|kind| !lead.contains(kind));
    lead.iter().copied().chain(rest).collect()
}

#[cfg(target_os = "horizon")]
mod ffi {
    extern "C" {
        /// `service_type` is a `PlServiceType`: 0, pl:u, serves fonts on every firmware.
        pub fn plInitialize(service_type: u32) -> u32;
        pub fn plExit();
        /// `font_type` is a `PlSharedFontType`; waits until the system has loaded it.
        pub fn plGetSharedFontByType(font: *mut super::PlFontData, font_type: u32) -> u32;
    }
}

/// Opens pl:u and returns the faces in [`font_order`] for `language`. A face the system does not
/// hand out is logged and left out. Call once.
#[cfg(target_os = "horizon")]
pub fn shared_fonts(language: &str) -> Vec<&'static [u8]> {
    // SAFETY: no pointers; balanced by `close`.
    let rc = unsafe { ffi::plInitialize(0) };
    if rc != 0 {
        tracing::warn!(result = %crate::result_code(rc), "shared fonts unavailable");
        return Vec::new();
    }
    let face = |kind: u32| {
        let mut font = PlFontData {
            kind,
            offset: 0,
            size: 0,
            address: core::ptr::null(),
        };
        // SAFETY: pl:u is open; `font` is a valid out-pointer.
        let rc = unsafe { ffi::plGetSharedFontByType(&mut font, kind) };
        if rc != 0 || font.address.is_null() || font.size == 0 {
            tracing::warn!(result = %crate::result_code(rc), "shared font {kind} unavailable");
            return None;
        }
        // SAFETY: libnx checked offset and size against the read-only mapping, which stays
        // until `close`, after the title's last code.
        Some(unsafe { core::slice::from_raw_parts(font.address, font.size as usize) })
    };
    font_order(language).into_iter().filter_map(face).collect()
}

/// Unmaps the shared fonts; a no-op when pl:u is not open. Only `rt::userAppExit` calls it.
#[cfg(target_os = "horizon")]
pub(crate) fn close() {
    // SAFETY: no title code runs after `userAppExit`, so no slice from `shared_fonts` is read.
    unsafe { ffi::plExit() };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_language_picks_the_first_face() {
        let first = |tag| font_order(tag)[0];
        for tag in ["en-US", "ja", "de", "pt-BR", ""] {
            assert_eq!(first(tag), STANDARD, "{tag}");
        }
        for tag in ["zh-CN", "zh-Hans", "zh", "zh-Hans-HK"] {
            assert_eq!(
                font_order(tag)[..2],
                [CHINESE_SIMPLIFIED, EXT_CHINESE_SIMPLIFIED],
                "{tag}"
            );
        }
        for tag in ["zh-TW", "zh-Hant", "ZH-hk", "zh-MO"] {
            assert_eq!(first(tag), CHINESE_TRADITIONAL, "{tag}");
        }
        assert_eq!(first("ko"), KO);
    }

    #[test]
    fn every_language_gets_every_text_face_once() {
        for tag in ["en-US", "ja", "zh-Hans", "zh-Hant", "ko"] {
            let mut order = font_order(tag);
            order.sort_unstable();
            assert_eq!(order, [0, 1, 2, 3, 4], "{tag}");
        }
    }
}
