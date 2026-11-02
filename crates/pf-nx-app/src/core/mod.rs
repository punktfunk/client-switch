pub mod caps;
pub mod dial;
pub mod errors;
pub mod input;
pub mod media;
pub mod model;
pub mod perf;
pub mod settings;

/// The packaged version (`1.0.0`, or `1.0.0+gabc1234` off a tag), threaded in by
/// `scripts/docker.sh` as `PKG_VERSION`;
/// a bare `cargo build` falls back to the crate version.
pub const VERSION: &str = match option_env!("PKG_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

/// The build id the About screen and the first log line carry: version plus the pins.
pub fn build_id() -> String {
    format!(
        "{VERSION} core={} mesa={}",
        option_env!("PF_CORE_REV").unwrap_or("dev"),
        option_env!("PF_MESA_PIN").unwrap_or("dev"),
    )
}
