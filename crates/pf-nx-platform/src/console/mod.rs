//! The console's implementations, assembled into one [`Platform`].

mod audio;
mod display_fb;
mod display_nvk;
mod lifecycle;
mod pads;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use pf_nx_app::platform::{read_wait_us, DeviceInfo, Display, OperationMode, Platform, Storage, VideoDecoders};
use pf_nx_sys::{applet, nvk, rt, setsys, socket, vi};
use pf_nx_vk::{GpuDevice, Presenter, Target, VideoSlot};

/// Identity, settings, logs and bundles; the NRO itself lives here too (plan D18).
const DATA_DIR: &str = "sdmc:/switch/punktfunk";

/// A file of this name in [`DATA_DIR`], read at start, turns the straight VIC route on, so one
/// build A/Bs both routes.
const STRAIGHT_MARKER: &str = "vic-direto";

/// A file of this name in [`DATA_DIR`], read at start, turns the mio leg's lean waker on.
const LEAN_WAKER_MARKER: &str = "despertador-leve";

/// A file of this name in [`DATA_DIR`], read at start, makes every batch read wait the
/// milliseconds it holds, so one build A/Bs several waits.
const READ_WAIT_MARKER: &str = "espera-leitura";

/// The Mesa pin NVK was built from, for the log and the About screen.
const MESA_PIN: &str = match option_env!("PF_MESA_PIN") {
    Some(p) => p,
    None => "unpinned",
};

/// What draws the console.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayKind {
    /// The presenter on NVK, with the stream layer.
    Nvk,
    /// libnx's linear framebuffer, CPU raster and no stream: the probe and the emulator.
    Fb,
}

struct Dir(PathBuf);

impl Storage for Dir {
    fn data_dir(&self) -> &Path {
        &self.0
    }
}

/// The operation mode in the app's vocabulary, as of the last applet pump.
fn operation_mode() -> OperationMode {
    match applet::operation_mode() {
        applet::OperationMode::Handheld => OperationMode::Handheld,
        applet::OperationMode::Docked => OperationMode::Docked,
    }
}

/// The stream and swapchain size each mode calls for (plan D11).
fn mode_size(mode: OperationMode) -> (u32, u32) {
    match mode {
        OperationMode::Handheld => (1280, 720),
        OperationMode::Docked => (1920, 1080),
    }
}

/// Starts the socket driver and nxlink once per process; the log copy when nxlink is up.
fn nxlink() -> Option<Box<dyn Write + Send>> {
    static UP: OnceLock<bool> = OnceLock::new();
    // TODO(verify): libnx's default buffers; probe 2 says whether UDP receive needs more for bursts.
    let up = *UP.get_or_init(|| match socket::init(&socket::Config::default()) {
        Ok(()) => socket::nxlink_stdio().is_some(),
        Err(e) => {
            println!("punktfunk: sockets: {e:#}");
            false
        }
    });
    up.then(|| Box::new(std::io::stdout()) as Box<dyn Write + Send>)
}

/// The markers' switches, read once: whether the waker is lean, and the read wait in microseconds.
fn socket_switches() -> (bool, u32) {
    static SWITCHES: OnceLock<(bool, u32)> = OnceLock::new();
    *SWITCHES.get_or_init(|| {
        let dir = Path::new(DATA_DIR);
        let wait = std::fs::read_to_string(dir.join(READ_WAIT_MARKER)).map_or(0, |text| read_wait_us(&text));
        (dir.join(LEAN_WAKER_MARKER).exists(), wait)
    })
}

/// Applies the markers. Before any thread exists: `set_var` is not thread-safe.
fn apply_socket_switches() {
    let (lean_waker, wait_us) = socket_switches();
    if lean_waker {
        std::env::set_var("PF_NX_LEAN_WAKER", "1");
    }
    rt::set_read_wait(Duration::from_micros(u64::from(wait_us)));
}

/// Builds the console platform. Sockets and nxlink come first, so a display that fails to come
/// up is reported; the display comes next, as the step most likely to fail.
pub fn platform(kind: DisplayKind) -> Result<Platform> {
    let log = nxlink();
    std::fs::create_dir_all(DATA_DIR).with_context(|| format!("create {DATA_DIR}"))?;
    apply_socket_switches();
    let (display, video, gpu, ui): (Box<dyn Display>, Arc<dyn VideoDecoders>, String, String) = match kind {
        DisplayKind::Nvk => {
            let window = vi::default_window();
            let mode = operation_mode();
            // No logger yet: nxlink's stdout is the only witness.
            println!("punktfunk: vi window {:?}, {mode:?}", vi::window_size(window));
            let target = Target::Vi {
                window,
                size: mode_size(mode),
                resize: vi::set_window_size,
            };
            let dev = Arc::new(GpuDevice::new(nvk::entry(), target).context("bring up the GPU")?);
            let slot = Arc::new(VideoSlot::default());
            let presenter = Presenter::new(dev.clone(), slot.clone()).context("bring up the display")?;
            let gpu = format!("NVK {MESA_PIN} ({})", dev.name);
            let ui = presenter.ui();
            (
                Box::new(display_nvk::NvkDisplay::new(presenter, mode)),
                Arc::new(
                    crate::video::NxVideo::gpu(dev, slot).straight(Path::new(DATA_DIR).join(STRAIGHT_MARKER).exists()),
                ),
                gpu,
                ui,
            )
        }
        DisplayKind::Fb => {
            let fb = display_fb::FbDisplay::new()?;
            let ui = fb.describe();
            (
                Box::new(fb),
                Arc::new(crate::video::NxVideo::cpu()),
                "none (framebuffer)".into(),
                ui,
            )
        }
    };
    let device = DeviceInfo {
        platform: "switch",
        model: setsys::product_model().unwrap_or("Nintendo Switch").into(),
        firmware: setsys::firmware_version().unwrap_or_default(),
        gpu,
        ui,
        applet_mode: !matches!(
            applet::applet_type(),
            applet::AppletType::Application | applet::AppletType::SystemApplication
        ),
    };
    Ok(Platform {
        display,
        video,
        audio: Arc::new(audio::NxAudio),
        pads: Box::new(pads::NxPads::new()?),
        storage: Arc::new(Dir(PathBuf::from(DATA_DIR))),
        lifecycle: Box::new(lifecycle::NxLifecycle::new()),
        log,
        device,
    })
}
