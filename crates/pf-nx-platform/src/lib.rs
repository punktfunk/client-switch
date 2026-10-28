//! The console's side of the Switch client's seams.
//!
//! The pure half (the pad map) builds and tests anywhere; everything that calls libnx, NVK or
//! libavcodec is `cfg(target_os = "horizon")`. The binary calls [`platform`] once and hands the
//! result to `pf_nx_app::run`.

pub mod pad_map;

#[cfg(target_os = "horizon")]
mod console;

#[cfg(target_os = "horizon")]
pub use console::{platform, DisplayKind};
