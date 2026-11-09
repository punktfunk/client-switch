//! The Switch client's Vulkan plane: one device for the title's life, shared by the presenter,
//! the UI layer (Skia) and the decode lanes.
//!
//! `unsafe` is allowed here and in `pf-nx-sys` only; every block carries its proof. The crate
//! builds on the console (NVK) and on Linux (lavapipe for the dev box), where `pf-client-video`
//! does.
#![cfg(any(target_os = "linux", target_os = "horizon"))]

pub mod device;
pub mod in_place;
pub mod output;
pub mod pacing;
pub mod picture;
pub mod pipelines;
pub mod presenter;
#[cfg(feature = "pyrowave")]
pub mod pyro;
mod timer;
pub mod ui;
pub mod util;

pub use device::{GpuDevice, Target, Vsync};
pub use in_place::{Check, InPlace, TiledSurface};
pub use pacing::Pacing;
pub use picture::{Drops, Nv12Upload, Picture, Planes, StagingPlanes, VideoSlot};
pub use presenter::Presenter;
#[cfg(feature = "pyrowave")]
pub use pyro::PyroLane;
