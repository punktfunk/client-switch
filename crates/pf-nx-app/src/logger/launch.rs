//! Launch parameters: argv[1] as JSON when `nxlink` passes one, else `launch.json` in the data
//! directory.
use std::sync::OnceLock;

use serde::Deserialize;

use tracing::Level;

/// All fields optional; a missing or unreadable source reads as defaults.
#[derive(Deserialize, Default)]
struct LaunchParams {
    /// `IP:PORT` of a `nc -lk` the log streams to.
    telemetry: Option<String>,
    telemetry_level: Option<String>,
}

fn launch_params() -> &'static LaunchParams {
    static PARAMS: OnceLock<LaunchParams> = OnceLock::new();
    PARAMS.get_or_init(|| {
        let raw = std::env::args()
            .nth(1)
            .filter(|arg| arg.starts_with('{'))
            .or_else(|| std::fs::read_to_string(crate::services::paths::app_dir().join("launch.json")).ok());
        raw.and_then(|raw| serde_json::from_str(&raw).ok()).unwrap_or_default()
    })
}

/// Where to stream logs beside the file; `None` means the file alone.
pub(super) fn telemetry_addr() -> Option<&'static str> {
    launch_params().telemetry.as_deref().filter(|s| !s.is_empty())
}

/// Launch-time log level; `None` keeps the default.
pub(super) fn launch_level() -> Option<Level> {
    match launch_params()
        .telemetry_level
        .as_deref()?
        .to_ascii_lowercase()
        .as_str()
    {
        "trace" => Some(Level::TRACE),
        "debug" => Some(Level::DEBUG),
        "info" => Some(Level::INFO),
        "warn" => Some(Level::WARN),
        "error" => Some(Level::ERROR),
        _ => None,
    }
}
