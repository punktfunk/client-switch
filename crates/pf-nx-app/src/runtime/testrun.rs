//! Scripted test runs: `task test-run` puts `test-run.json` into the data directory, the next
//! launch reads and deletes it, runs the scenario on its own, and leaves a bundle in
//! `verdict/<unix-seconds>/` beside it for `task fetch-bundle`: frames as PNG, `perf.json`,
//! `environment.json`, `verdict.json` and the log.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{App, UiOutcome};
use crate::platform::Rgba;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Scenario {
    /// Draw the console for a few seconds, capture it, leave.
    Menu,
    /// Connect to `host` with the saved pairing, stream for `seconds`, capture, leave.
    Stream,
}

/// Frames the menu scenario draws before it captures: four seconds at 60 Hz.
const MENU_FRAMES: u64 = 240;

#[derive(Clone, Debug, Deserialize)]
pub struct TestRun {
    pub scenario: Scenario,
    /// A paired host's address; the stream scenario's target.
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    /// The PIN on the host's Pairing page: the run pairs with `host` first when it is not paired
    /// here yet, since nobody is at the pad to type it.
    #[serde(default)]
    pub pin: Option<String>,
    #[serde(default = "default_seconds")]
    pub seconds: u64,
    /// Seconds into the stream at which to save a frame.
    #[serde(default)]
    pub captures: Vec<u64>,
    /// Stream settings for this run only, in a profile's sparse form, laid over the ones the
    /// launch resolves. Nothing is saved, so the next launch streams with the user's settings.
    #[serde(default)]
    pub settings: Option<pf_client_core::presets::SettingsOverlay>,
    #[serde(skip)]
    pub bundle: PathBuf,
}

fn default_seconds() -> u64 {
    60
}

/// Reads and deletes `test-run.json`, so a crash mid-run does not loop on the next launch.
pub fn take() -> Option<TestRun> {
    let dir = crate::services::paths::app_dir();
    let path = dir.join("test-run.json");
    let raw = std::fs::read(&path).ok()?;
    if let Err(e) = std::fs::remove_file(&path) {
        tracing::warn!("test run: remove {}: {e}", path.display());
    }
    let mut run: TestRun = match serde_json::from_slice(&raw) {
        Ok(run) => run,
        Err(e) => {
            tracing::error!("test run: parse test-run.json: {e}");
            return None;
        }
    };
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    run.bundle = dir.join("verdict").join(stamp.to_string());
    if let Err(e) = std::fs::create_dir_all(&run.bundle) {
        tracing::error!("test run: create {}: {e}", run.bundle.display());
        return None;
    }
    tracing::info!(
        "test run: {:?} for {}s into {}",
        run.scenario,
        run.seconds,
        run.bundle.display()
    );
    Some(run)
}

impl TestRun {
    /// Saves a frame as `frame-<name>.png`.
    pub fn capture(&self, name: &str, frame: Rgba) -> Result<()> {
        let path = self.bundle.join(format!("frame-{name}.png"));
        let image = image::RgbaImage::from_raw(frame.width, frame.height, frame.pixels)
            .context("read-back size does not match its pixels")?;
        image.save(&path).with_context(|| format!("write {}", path.display()))
    }

    pub fn write_json(&self, name: &str, value: &serde_json::Value) -> Result<()> {
        let path = self.bundle.join(name);
        let text = serde_json::to_string_pretty(value).context("serialize")?;
        crate::services::atomic::write(&path, &text, name)
    }

    /// Writes the verdict and the environment, and copies the log in.
    pub(super) fn finish(&self, app: &App, ok: bool, detail: &str) {
        let verdict = serde_json::json!({
            "scenario": self.scenario,
            "ok": ok,
            "detail": detail,
            "build": crate::core::build_id(),
        });
        let (width, height) = app.display.size();
        let environment = serde_json::json!({
            "platform": app.device.platform,
            "model": app.device.model,
            "firmware": app.device.firmware,
            "gpu": app.device.gpu,
            "ui": app.device.ui,
            "output": { "width": width, "height": height, "refresh_mhz": app.display.refresh().0 },
            "operation_mode": format!("{:?}", app.display.mode()),
        });
        for (name, value) in [("verdict.json", &verdict), ("environment.json", &environment)] {
            if let Err(e) = self.write_json(name, value) {
                tracing::warn!("test run: {name}: {e:#}");
            }
        }
        tracing::info!("test run: {} — {detail}", if ok { "ok" } else { "FAILED" });
        copy_log(&self.bundle);
    }

    /// Whether `elapsed` just crossed one of [`Self::captures`].
    pub fn capture_due(&self, before: Duration, after: Duration) -> Option<u64> {
        self.captures
            .iter()
            .copied()
            .find(|&s| before < Duration::from_secs(s) && after >= Duration::from_secs(s))
    }
}

/// The menu scenario's step: after [`MENU_FRAMES`], capture and leave.
pub(super) fn menu_step(app: &mut App, frames: u64) -> Option<UiOutcome> {
    let run = app.test_run.as_ref()?;
    if run.scenario != Scenario::Menu || frames < MENU_FRAMES {
        return None;
    }
    let run = app.test_run.take()?;
    let ok = match app.display.read_back() {
        Ok(Some(frame)) => run.capture("menu", frame).map_err(|e| format!("{e:#}")),
        Ok(None) => Err("no read-back on this display".into()),
        Err(e) => Err(format!("{e:#}")),
    };
    match ok {
        Ok(()) => run.finish(app, true, "menu drawn and captured"),
        Err(e) => run.finish(app, false, &e),
    }
    Some(UiOutcome::Quit(None))
}

/// The log as it stands, best-effort: lines still queued in the appender are missing.
fn copy_log(bundle: &Path) {
    let logs = crate::services::paths::logs_dir();
    if let Some(log) = crate::logger::latest_log_file(&logs) {
        if let Err(e) = std::fs::copy(&log, bundle.join("log.txt")) {
            tracing::warn!("test run: copy log: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scenario_file_reads_with_defaults() {
        let run: TestRun =
            serde_json::from_str(r#"{"scenario":"stream","host":"10.0.0.2","captures":[5,30]}"#).unwrap();
        assert_eq!(run.scenario, Scenario::Stream);
        assert_eq!(run.seconds, 60);
        assert_eq!(
            run.capture_due(Duration::from_millis(4_990), Duration::from_millis(5_010)),
            Some(5)
        );
        assert_eq!(run.capture_due(Duration::from_secs(6), Duration::from_secs(7)), None);
        assert!(run.settings.is_none());
        assert!(run.pin.is_none());
    }

    #[test]
    fn a_scenario_file_carries_the_pin_to_pair_with() {
        let run: TestRun = serde_json::from_str(r#"{"scenario":"stream","host":"10.0.0.2","pin":"123456"}"#).unwrap();
        assert_eq!(run.pin.as_deref(), Some("123456"));
    }

    #[test]
    fn a_scenario_file_carries_settings_for_the_run() {
        let run: TestRun = serde_json::from_str(
            r#"{"scenario":"stream","host":"10.0.0.2","settings":{"codec":"hevc","bitrate_kbps":60000,"refresh_hz":120}}"#,
        )
        .unwrap();
        let overlay = run.settings.expect("settings");
        let base = pf_client_core::trust::Settings::default();
        let applied = overlay.apply(&base);
        assert_eq!(applied.codec, "hevc");
        assert_eq!(applied.bitrate_kbps, 60_000);
        assert_eq!(applied.refresh_hz, 120);
        assert_eq!(applied.width, base.width);
    }
}
