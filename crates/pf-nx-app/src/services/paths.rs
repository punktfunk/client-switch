//! Where this app keeps its files: the platform's data directory (`sdmc:/switch/punktfunk`),
//! installed once at startup. Unset reads as the system temp dir, so tests write nowhere odd.
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static APP_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Publish the data directory. Call once, before anything reads or writes a file.
pub fn install(dir: &Path) {
    if APP_DIR.set(dir.to_path_buf()).is_err() {
        tracing::warn!("data dir already installed — ignoring {}", dir.display());
    }
}

pub fn app_dir() -> PathBuf {
    APP_DIR.get().cloned().unwrap_or_else(std::env::temp_dir)
}

/// Where the rotating logs live, created on first use.
pub fn logs_dir() -> PathBuf {
    let dir = app_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// The one charset every packaged asset name and every mDNS-advertised OS token is held to.
pub fn is_asset_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
}
