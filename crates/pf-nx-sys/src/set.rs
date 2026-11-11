//! The system's language. Externs from `services/set.h`, libnx 4.12.0. This is the `set` service
//! any title may open, not `set:sys` (see [`crate::setsys`]); libnx counts the opens.

/// A `LanguageCode` as a BCP-47 tag: up to eight ASCII bytes, NUL-padded, packed little-endian
/// into a `u64` (`en-US`, `es-419`, `zh-Hans`). `None` for an empty or non-tag value.
pub fn language_tag(code: u64) -> Option<String> {
    let bytes = code.to_le_bytes();
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let tag = &bytes[..len];
    let valid = !tag.is_empty() && tag.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'-');
    valid.then(|| tag.iter().map(|&b| char::from(b)).collect())
}

#[cfg(target_os = "horizon")]
mod ffi {
    extern "C" {
        pub fn setInitialize() -> u32;
        pub fn setExit();
        pub fn setGetSystemLanguage(language_code: *mut u64) -> u32;
    }
}

/// The system's UI language as a list of BCP-47 tags, most preferred first. The console has one;
/// the list is empty where the service does not answer.
#[cfg(target_os = "horizon")]
pub fn system_languages() -> Vec<String> {
    // SAFETY: no arguments; balanced by setExit.
    if unsafe { ffi::setInitialize() } != 0 {
        return Vec::new();
    }
    let mut code = 0u64;
    // SAFETY: set is open; `code` is a valid out-pointer.
    let rc = unsafe { ffi::setGetSystemLanguage(&mut code) };
    // SAFETY: balances the setInitialize above.
    unsafe { ffi::setExit() };
    if rc != 0 {
        return Vec::new();
    }
    language_tag(code).into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(tag: &str) -> u64 {
        let mut bytes = [0u8; 8];
        bytes[..tag.len()].copy_from_slice(tag.as_bytes());
        u64::from_le_bytes(bytes)
    }

    /// Every `LanguageCode` a Switch 1 offers, `SetLanguage` 0 to 17. `zh-CN` and `zh-TW` predate
    /// `zh-Hans` and `zh-Hant` (4.0.0); pf-l10n reads a `zh` tag with no script by its region.
    const CODES: [&str; 18] = [
        "ja", "en-US", "fr", "de", "it", "es", "zh-CN", "ko", "nl", "pt", "ru", "zh-TW", "en-GB", "fr-CA", "es-419",
        "zh-Hans", "zh-Hant", "pt-BR",
    ];

    #[test]
    fn every_language_code_unpacks_to_its_tag() {
        for code in CODES {
            assert_eq!(language_tag(pack(code)).as_deref(), Some(code));
        }
    }

    #[test]
    fn a_code_that_is_not_a_tag_is_none() {
        assert_eq!(language_tag(0), None);
        assert_eq!(language_tag(u64::MAX), None);
    }
}
