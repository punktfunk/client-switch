//! The Switch client's application, portable and free of `unsafe`.
//!
//! Layered, dependencies pointing inward: `core` (domain, no I/O) ← `services` (store, discovery,
//! pinned TLS, art) ← `session` (streaming on punktfunk-core) ← `ui` (the shared console's
//! service) ← `runtime` (the menu and stream loops). `platform` holds the seams the binary fills
//! with the console's or a dev box's implementations; nothing here names either.
#![forbid(unsafe_code)]

pub mod core;
pub mod logger;
pub mod platform;
pub mod runtime;
pub mod services;
pub mod session;
pub mod ui;

pub use runtime::run;

/// The process-default TLS provider ureq's agents resolve; `run` installs it, a probe calls this.
pub fn install_tls_provider() {
    punktfunk_core::tls::install_default_provider();
}
