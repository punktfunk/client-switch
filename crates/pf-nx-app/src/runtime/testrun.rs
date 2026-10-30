//! Scripted test runs: `task test-run` puts `test-run.json` into the data directory, the next
//! launch reads and deletes it, runs the scenario on its own, and leaves a bundle in
//! `verdict/<unix-seconds>/` beside it for `task fetch-bundle`: frames as PNG, `perf.json`,
//! `environment.json`, `verdict.json` and the log.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{App, UiOutcome};
use crate::platform::{Display, PadSample, Rgba};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Scenario {
    /// Draw the console for a few seconds, or press through [`TestRun::pads`], capture it, leave.
    Menu,
    /// Connect to `host` with the saved pairing, stream for `seconds`, capture, leave.
    Stream,
}

/// Frames the menu scenario draws before it captures: four seconds at 60 Hz.
const MENU_FRAMES: u64 = 240;

#[derive(Clone, Debug, Deserialize)]
pub struct TestRun {
    pub scenario: Scenario,
    /// A paired host's address: the stream scenario's target, the menu scenario's to pair with.
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    /// The PIN on the host's Pairing page: the run pairs with `host` first when it is not paired
    /// here yet, since nobody is at the pad to type it.
    #[serde(default)]
    pub pin: Option<String>,
    /// `host`'s management port, saved on its record as its advert would: where mDNS does not
    /// reach (the dev box in a container) the library otherwise asks the default port.
    #[serde(default)]
    pub mgmt_port: Option<u16>,
    #[serde(default = "default_seconds")]
    pub seconds: u64,
    /// Seconds into the stream at which to save a frame.
    #[serde(default)]
    pub captures: Vec<u64>,
    /// Stream settings for this run only, in a profile's sparse form, laid over the ones the
    /// launch resolves. Nothing is saved, so the next launch streams with the user's settings.
    #[serde(default)]
    pub settings: Option<pf_client_core::presets::SettingsOverlay>,
    /// The menu scenario's pad, step by step, in place of the frame count: nobody is at the pad.
    #[serde(default)]
    pub pads: Vec<PadStep>,
    #[serde(skip)]
    pub bundle: PathBuf,
    #[serde(skip)]
    cursor: PadCursor,
}

fn default_seconds() -> u64 {
    60
}

/// One press: the buttons down for a frame, then `wait` frames with none, then a capture as
/// `frame-<capture>.png` when named. An empty `press` only waits.
#[derive(Clone, Debug, Deserialize)]
pub struct PadStep {
    #[serde(default)]
    pub press: Vec<Button>,
    #[serde(default = "default_wait")]
    pub wait: u64,
    #[serde(default)]
    pub capture: Option<String>,
}

fn default_wait() -> u64 {
    30
}

/// The buttons by their Nintendo labels.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Button {
    A,
    B,
    X,
    Y,
    L,
    R,
    Plus,
    Minus,
    Up,
    Down,
    Left,
    Right,
}

impl Button {
    fn bit(self) -> u32 {
        use punktfunk_core::input::gamepad;
        let [a, b, x, y] = super::pads::FACE;
        match self {
            Self::A => a,
            Self::B => b,
            Self::X => x,
            Self::Y => y,
            Self::L => gamepad::BTN_LB,
            Self::R => gamepad::BTN_RB,
            Self::Plus => gamepad::BTN_START,
            Self::Minus => gamepad::BTN_BACK,
            Self::Up => gamepad::BTN_DPAD_UP,
            Self::Down => gamepad::BTN_DPAD_DOWN,
            Self::Left => gamepad::BTN_DPAD_LEFT,
            Self::Right => gamepad::BTN_DPAD_RIGHT,
        }
    }
}

/// Where [`TestRun::pads`] stands: the step, and the frames drawn in it.
#[derive(Clone, Copy, Debug, Default)]
struct PadCursor {
    step: usize,
    frame: u64,
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

    /// The scripted pad for the frame about to be drawn, in slot 0; `None` without a script or
    /// once it has run out.
    pub fn pad_sample(&self) -> Option<PadSample> {
        let step = self.pads.get(self.cursor.step)?;
        let buttons = if self.cursor.frame == 0 {
            step.press.iter().fold(0, |bits, b| bits | b.bit())
        } else {
            0
        };
        Some(PadSample {
            buttons,
            ..PadSample::default()
        })
    }

    /// Counts a drawn frame against the script: the capture name when a step just ended with
    /// one, and whether the script has now run out.
    fn pad_frame_drawn(&mut self) -> (Option<String>, bool) {
        let Some(step) = self.pads.get(self.cursor.step) else {
            return (None, true);
        };
        self.cursor.frame += 1;
        if self.cursor.frame <= step.wait {
            return (None, false);
        }
        let capture = step.capture.clone();
        self.cursor = PadCursor {
            step: self.cursor.step + 1,
            frame: 0,
        };
        (capture, self.cursor.step >= self.pads.len())
    }

    /// Whether `elapsed` just crossed one of [`Self::captures`].
    pub fn capture_due(&self, before: Duration, after: Duration) -> Option<u64> {
        self.captures
            .iter()
            .copied()
            .find(|&s| before < Duration::from_secs(s) && after >= Duration::from_secs(s))
    }
}

/// The menu scenario's step after each frame: the pad script's captures as they come, then,
/// once the script has run out (or after [`MENU_FRAMES`] without one), capture and leave.
pub(super) fn menu_step(app: &mut App, frames: u64) -> Option<UiOutcome> {
    let run = app.test_run.as_mut().filter(|r| r.scenario == Scenario::Menu)?;
    let done = if run.pads.is_empty() {
        frames >= MENU_FRAMES
    } else {
        let (capture, done) = run.pad_frame_drawn();
        if let Some(name) = capture {
            if let Err(e) = capture_now(run, &mut *app.display, &name) {
                let run = app.test_run.take()?;
                run.finish(app, false, &format!("capture {name}: {e}"));
                return Some(UiOutcome::Quit(None));
            }
        }
        done
    };
    if !done {
        return None;
    }
    let run = app.test_run.take()?;
    match capture_now(&run, &mut *app.display, "menu") {
        Ok(()) => run.finish(app, true, "menu drawn and captured"),
        Err(e) => run.finish(app, false, &e),
    }
    Some(UiOutcome::Quit(None))
}

fn capture_now(run: &TestRun, display: &mut dyn Display, name: &str) -> Result<(), String> {
    match display.read_back() {
        Ok(Some(frame)) => run.capture(name, frame).map_err(|e| format!("{e:#}")),
        Ok(None) => Err("no read-back on this display".into()),
        Err(e) => Err(format!("{e:#}")),
    }
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
    fn a_pad_script_presses_once_per_step_and_captures_at_its_end() {
        let mut run: TestRun = serde_json::from_str(
            r#"{"scenario":"menu","pads":[{"press":["right","a"],"wait":2,"capture":"next"},{"press":["b"],"wait":0}]}"#,
        )
        .unwrap();
        let [a, b, ..] = super::super::pads::FACE;
        let right = punktfunk_core::input::gamepad::BTN_DPAD_RIGHT;
        let mut seen = Vec::new();
        while let Some(sample) = run.pad_sample() {
            let (capture, done) = run.pad_frame_drawn();
            seen.push((sample.buttons, capture, done));
        }
        assert_eq!(
            seen,
            [
                (right | a, None, false),
                (0, None, false),
                (0, Some("next".into()), false),
                (b, None, true),
            ]
        );
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
