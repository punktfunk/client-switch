//! The console's C ABI, hand-written: libnx 4.12.0, libavcodec with the `nvtegra` hwaccel behind
//! one C shim (`csrc/pf_av.c`), and the statically linked NVK's entry point.
//!
//! Every `#[repr(C)]` struct is pinned by `tests/layout.rs` at the size and offsets derived from
//! the libnx 4.12.0 headers; `tools/gen-layout.sh` prints the same numbers through bindgen.
//! Everything that calls the console is `cfg(target_os = "horizon")`: off the console only the
//! pure types and maps build, and `av` decodes in software on Linux (the dev box) and opens to an
//! error elsewhere. Each module names the header its
//! externs come from.
//!
//! Conventions: a libnx `Result` is a `u32`, zero on success; a C enum is a 32-bit integer.
#![allow(non_camel_case_types, non_snake_case)]

pub mod applet;
pub mod audout;
// `av` and `nvk` are features: the probe links neither libavcodec nor NVK.
#[cfg(feature = "av")]
pub mod av;
pub mod clk;
#[cfg(target_os = "horizon")]
pub mod console;
pub mod fb;
pub mod hid;
pub mod mem;
#[cfg(all(target_os = "horizon", feature = "nvk"))]
pub mod nvk;
pub mod random;
#[cfg(target_os = "horizon")]
pub mod rt;
pub mod setsys;
pub mod socket;
pub mod swkbd;
pub mod thread;
pub mod vi;

/// A libnx `Result` as the console's error screens spell it: `2000 + module`, then the
/// description (result.h, `R_MODULE` / `R_DESCRIPTION`).
pub fn result_code(rc: u32) -> String {
    format!("{:04}-{:04}", 2000 + (rc & 0x1FF), (rc >> 9) & 0x1FFF)
}

/// The operator error for a libnx call that returned `rc`: `"{op}: 2001-0106"`.
#[cfg_attr(not(target_os = "horizon"), allow(dead_code))]
pub(crate) fn nx_error(op: &str, rc: u32) -> anyhow::Error {
    anyhow::anyhow!("{op}: {}", result_code(rc))
}

#[cfg(test)]
mod tests {
    #[test]
    fn result_code_matches_the_error_screen() {
        // KERNELRESULT(InvalidMemoryState), result.h: module 1, description 106.
        assert_eq!(super::result_code(0xD401), "2001-0106");
        assert_eq!(super::result_code(0), "2000-0000");
    }
}
