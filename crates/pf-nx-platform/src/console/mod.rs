//! The console's implementations, assembled into one [`Platform`].

mod audio;
mod display_fb;
mod display_nvk;
mod lifecycle;
mod pads;
pub(crate) mod threads;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use pf_nx_app::platform::{DeviceInfo, Display, OperationMode, Platform, Storage, VideoDecoders};
use pf_nx_sys::{applet, nvk, set, setsys, socket, vi};
use pf_nx_vk::{GpuDevice, Pacing, Presenter, Target, VideoSlot, Vsync};

/// Identity, settings, logs and bundles; the NRO itself lives here too (plan D18).
const DATA_DIR: &str = "sdmc:/switch/punktfunk";

/// A file of this name in [`DATA_DIR`], read at start, turns the straight VIC route on, so one
/// build A/Bs both routes.
const STRAIGHT_MARKER: &str = "vic-direto";
/// A file of this name keeps pictures on the VIC route instead of in place, for the same A/B.
const VIC_MARKER: &str = "vic-copia";
/// A file of this name paces the presenter by the acquire, two frames queued, for the A/B of the
/// vblank pacing.
const ACQUIRE_MARKER: &str = "fila-antiga";
/// A file of this name paces on the vblank but takes the picture at once, for the same A/B.
const NO_LATE_MARKER: &str = "sem-espera";

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

/// On mains power: NVDEC may hold its top clock. Handheld keeps nvtegra's scaling.
fn docked() -> bool {
    applet::operation_mode() == applet::OperationMode::Docked
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

/// Builds the console platform. Core's thread boost, sockets and nxlink come first, so a display
/// that fails to come up is reported; the display comes next, as the step most likely to fail.
pub fn platform(kind: DisplayKind) -> Result<Platform> {
    threads::install();
    let log = nxlink();
    std::fs::create_dir_all(DATA_DIR).with_context(|| format!("create {DATA_DIR}"))?;
    let (display, video, gpu, ui): (Box<dyn Display>, Arc<dyn VideoDecoders>, String, String) = match kind {
        DisplayKind::Nvk => {
            let window = vi::default_window();
            let mode = operation_mode();
            // No logger yet: nxlink's stdout is the only witness.
            println!("punktfunk: vi window {:?}, {mode:?}", vi::window_size(window));
            let vsync = match vi::open_vsync() {
                Ok(()) => Some(Vsync {
                    clear: vi::clear_vsync,
                    wait: vi::wait_vsync,
                }),
                Err(e) => {
                    println!("punktfunk: no vsync event: {e:#}");
                    None
                }
            };
            let target = Target::Vi {
                window,
                size: mode_size(mode),
                resize: vi::set_window_size,
                vsync,
                queued: vi::queue_behind,
            };
            let dev = Arc::new(GpuDevice::new(nvk::entry(), target).context("bring up the GPU")?);
            let slot = Arc::new(VideoSlot::default());
            let mut presenter = Presenter::new(dev.clone(), slot.clone()).context("bring up the display")?;
            let gpu = format!("NVK {MESA_PIN} ({})", dev.name);
            let ui = presenter.ui();
            let marker = |name: &str| Path::new(DATA_DIR).join(name).exists();
            presenter.set_pacing(if marker(ACQUIRE_MARKER) {
                Pacing::Acquire
            } else if marker(NO_LATE_MARKER) {
                Pacing::Vblank
            } else {
                Pacing::Late
            });
            (
                Box::new(display_nvk::NvkDisplay::new(presenter, mode)),
                Arc::new(
                    crate::video::NxVideo::gpu(dev, slot)
                        .straight(marker(STRAIGHT_MARKER))
                        .in_place(!marker(VIC_MARKER))
                        .top_clock(docked),
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
        languages: set::system_languages(),
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
