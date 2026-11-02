//! Persistence: the `settings.json` document, plus the client identity PEMs beside it.
//!
//! - [`load`] / [`save`] read and write the whole [`Persisted`] document.
//! - [`StateWriter`] is what the app actually saves through: off-thread, coalescing.
//! - [`load_or_create_identity`] handles the PEM pair, which stays outside the document.
pub mod console;
mod identity;
pub mod shared;
mod writer;

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::core::VERSION;

pub use crate::core::model::{
    follow_moved_host, seed_new_host_profiles, upsert_known_host, CodecPref, ExitAction, GamepadType, Persisted,
};
pub use crate::core::settings::NxSettings;
pub use crate::services::paths::app_dir;
pub use identity::load_or_create_identity;
pub use pf_client_core::trust::Settings;
pub use writer::StateWriter;

fn path() -> PathBuf {
    app_dir().join("settings.json")
}

/// Loads the whole persisted document. Absent, unreadable and unparseable all answer with
/// defaults — a torn file must not take the app down (`services::atomic` is what prevents one).
pub fn load() -> Persisted {
    // No migration — a document that doesn't deserialize answers with defaults.
    let mut state = std::fs::read(path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Persisted>(&bytes).ok())
        .unwrap_or_default();
    stamp_version(&mut state);
    // A document written by another client can hold picks this console cannot present.
    state.settings.clamp_to_caps();
    state
}

/// Brings [`Persisted::version`] up to this build's.
///
/// Written synchronously so it happens once — `StateWriter`'s baseline is taken after this,
/// and an unstamped document that never gets saved would stamp again on every launch.
fn stamp_version(state: &mut Persisted) {
    if state.version.as_deref() == Some(VERSION) {
        return;
    }
    state.version = Some(VERSION.to_string());
    match save(state) {
        Ok(()) => tracing::info!("stamped settings.json with version {VERSION}"),
        Err(e) => tracing::warn!("stamp document version: {e:#}"),
    }
}

/// Writes the whole document: its `settings` object is the shared schema verbatim (see [`shared`]).
pub fn save(state: &Persisted) -> Result<()> {
    let json = serde_json::to_string_pretty(state).context("serialize app state")?;
    crate::services::atomic::write(&path(), &json, "settings.json")
}
