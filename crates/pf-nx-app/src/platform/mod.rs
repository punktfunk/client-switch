//! The seams between the application and whatever it runs on.
//!
//! Three implementations exist: the console (`pf-nx-platform`), a developer box
//! (`pf-nx-host`) and [`fake`] for tests. The binary builds one [`Platform`] and hands it to
//! [`crate::run`]; nothing in this crate names a console API, a Vulkan handle or a file path
//! outside [`Storage::data_dir`]. The contract tests in `tests/seams.rs` are the spec.

pub mod fake;

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use punktfunk_core::quic;

use crate::core::caps::VideoCaps;
use crate::core::media::VideoSink;

/// Everything the application needs from below, built once by the binary.
pub struct Platform {
    pub display: Box<dyn Display>,
    pub video: Arc<dyn VideoDecoders>,
    pub audio: Arc<dyn AudioOut>,
    pub pads: Box<dyn Pads>,
    pub storage: Arc<dyn Storage>,
    pub lifecycle: Box<dyn Lifecycle>,
    /// A second log destination beside the file: nxlink's stdout on the console, stderr on a
    /// dev box.
    pub log: Option<Box<dyn Write + Send>>,
    pub device: DeviceInfo,
}

/// What the logs and the About screen say about the machine.
#[derive(Clone, Debug, Default)]
pub struct DeviceInfo {
    /// `switch`, `host` or `fake`.
    pub platform: &'static str,
    pub model: String,
    pub firmware: String,
    /// The graphics driver and its pin (`NVK <mesa pin>`).
    pub gpu: String,
    /// How the UI layer draws: backend, size, and why it fell back to the CPU.
    pub ui: String,
    /// Launched as an applet (from the album), with a fraction of the title's memory (plan D13).
    pub applet_mode: bool,
    /// The system's UI language as BCP-47 tags, most preferred first. Empty reads as English.
    pub languages: Vec<String>,
}

/// Refresh rate of the scan-out, in millihertz (60 000, 59 940).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refresh(pub u32);

impl Refresh {
    pub fn hz(self) -> u32 {
        (self.0 + 500) / 1000
    }

    pub fn period_ns(self) -> u64 {
        1_000_000_000_000 / u64::from(self.0.max(1))
    }
}

/// How a stream picture sits in the output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fit {
    /// Scaled to fit with bars; never cropped.
    #[default]
    Contain,
    /// Scaled to fill; the overflow is cropped.
    Cover,
    /// Stretched to the output.
    Stretch,
}

/// The console's operation mode; the stream mode follows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OperationMode {
    Handheld,
    #[default]
    Docked,
}

/// The one output and its frame loop. Lives on the main thread: Skia is not `Send`.
pub trait Display {
    /// The swapchain extent (1280×720 handheld, 1920×1080 docked on the console).
    fn size(&self) -> (u32, u32);

    /// The UI canvas [`Self::frame`] hands out; may be smaller than the output and scaled up.
    fn ui_size(&self) -> (u32, u32) {
        self.size()
    }

    fn refresh(&self) -> Refresh;

    /// Handheld or docked. A dev box is docked.
    fn mode(&self) -> OperationMode {
        OperationMode::Docked
    }

    /// Draws one frame and presents it at the next vblank: the newest due stream picture
    /// (when [`Self::set_stream`] is on), then `ui` over it on a transparent layer while
    /// [`Self::set_ui_layer`] has it on. Blocks until a swapchain image is free, so this paces
    /// the main loop.
    fn frame(&mut self, ui: &mut dyn FnMut(&skia_safe::Canvas)) -> Result<FrameReport>;

    /// Whether the stream layer shows. Off, the UI draws over black and the UI layer is on.
    fn set_stream(&mut self, on: bool, fit: Fit);

    /// Whether [`Self::frame`] draws and blends the UI layer. A stream turns it off while its
    /// overlay is empty, which spares a full-screen clear, flush and blend each frame.
    fn set_ui_layer(&mut self, _on: bool) {}

    /// The last presented frame as tightly packed RGBA, for test-run bundles.
    fn read_back(&mut self) -> Result<Option<Rgba>>;
}

/// What one presented frame cost.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameReport {
    /// A vblank went by with nothing new presented.
    pub missed: bool,
    /// The stream picture shown this frame was new.
    pub new_picture: bool,
    /// Decode-complete to present, for the picture shown, when it was new.
    pub decode_to_present_us: Option<u32>,
    /// Decoded pictures dropped unseen since the last frame, passed over for a newer one.
    pub replaced: u32,
    /// Decoded pictures dropped unseen since the last frame, pushed out of a full smoothing queue.
    pub overflow: u32,
    /// How long an earlier frame's composition ran on the GPU (the one whose slot this frame
    /// reused), where the GPU can time it.
    pub gpu_us: Option<u32>,
}

/// A read-back frame.
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// The decode lanes. A sink opened here feeds the [`Display`]'s stream layer.
pub trait VideoDecoders: Send + Sync {
    /// What this device decodes; read once at startup into [`crate::core::caps`].
    fn caps(&self) -> VideoCaps;

    /// Opens the decoder for the negotiated stream (`codec` is one `quic::CODEC_*` bit).
    fn open(&self, spec: &StreamSpec) -> Result<Box<dyn VideoSink>>;

    /// The decoder [`Self::open`] picks for `codec`, as the stats overlay names it.
    fn lane_name(&self, codec: u8) -> &'static str;
}

/// What the handshake settled on, as a decoder needs it.
#[derive(Clone)]
pub struct StreamSpec {
    pub codec: u8,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
    pub color: quic::ColorInfo,
    pub bit_depth: u8,
    pub chroma444: bool,
    /// Bytes per shard on the wire, which a chunk-aligned AU is cut into.
    pub shard_payload: usize,
    /// The smoothness budget in frames, `1..=3`; `0` is the latency intent (newest picture wins).
    pub smoothing: usize,
    /// The session's overlay figures: lanes note decode, the presenter notes display.
    pub hud: Arc<punktfunk_core::hud::Stats>,
    /// Where the presenter leaves each picture's capture-to-glass time for the audio's A/V
    /// sync measure.
    pub video_e2e: VideoE2e,
}

/// The video's capture-to-glass time in core's cells: the presenter writes it, the audio route
/// compares its own against it. `0` until a picture reaches the glass.
#[derive(Clone, Debug, Default)]
pub struct VideoE2e {
    e2e_ns: Arc<AtomicU64>,
    /// Host clock minus this one, kept current by core.
    clock_offset_ns: Arc<AtomicI64>,
}

/// A capture-to-glass time past this is a clock step, not a picture.
const E2E_SANE_NS: u64 = 10_000_000_000;

impl VideoE2e {
    pub fn new(e2e_ns: Arc<AtomicU64>, clock_offset_ns: Arc<AtomicI64>) -> Self {
        Self {
            e2e_ns,
            clock_offset_ns,
        }
    }

    /// A picture captured at host time `pts_ns` reached the glass at `displayed_ns`, this
    /// clock's realtime.
    pub fn note(&self, pts_ns: u64, displayed_ns: u64) {
        let offset = self.clock_offset_ns.load(Ordering::Relaxed);
        let e2e = i128::from(displayed_ns) + i128::from(offset) - i128::from(pts_ns);
        if let Ok(ns) = u64::try_from(e2e) {
            if ns > 0 && ns < E2E_SANE_NS {
                self.e2e_ns.store(ns, Ordering::Relaxed);
            }
        }
    }
}

/// The console's audio output. Opening a port is cheap; a session holds one.
pub trait AudioOut: Send + Sync {
    fn open(&self, channels: u8, rate: u32) -> Result<Box<dyn AudioPort>>;
}

/// One open output port. Interleaved f32 at the opened rate and width.
pub trait AudioPort: Send {
    /// Frames per [`Self::write`]; fixed for the port's life.
    fn grain_frames(&self) -> usize;

    /// Plays exactly one grain, blocking until the device takes it.
    fn write(&mut self, pcm: &[f32]) -> Result<()>;

    /// Frames handed to the device and not yet played, which the next grain queues behind.
    fn queued_frames(&self) -> usize {
        0
    }
}

/// One pad's state at one instant, already in punktfunk's wire vocabulary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PadSample {
    /// Stable slot, 0..4: one per signed-in user.
    pub slot: u8,
    pub kind: PadKind,
    /// `punktfunk_core::input::gamepad::BTN_*` bits.
    pub buttons: u32,
    /// Sticks in the wire's range and sign (+y is up).
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
    pub lt: u8,
    pub rt: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PadKind {
    #[default]
    Other,
    /// Two Joy-Cons used as one pad: attached in handheld, or held one in each hand.
    JoyConPair,
    /// One Joy-Con held sideways.
    JoyConLeft,
    JoyConRight,
    SwitchPro,
}

/// One motion sample in the wire's frame, SDL's: +x right, +y up, +z toward the player.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Motion {
    /// Rotation about each axis, rad/s.
    pub gyro: [f32; 3],
    /// In g; a pad lying still reads +1 on y.
    pub accel: [f32; 3],
}

/// One finger on the console's own screen.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TouchPoint {
    /// Stable while the finger stays down.
    pub id: u32,
    /// 0.0..=1.0 across the screen from its top-left corner.
    pub x: f32,
    pub y: f32,
}

/// What the host asked a pad to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PadFeedback {
    /// Motor levels, 0..=65535.
    Rumble { low: u16, high: u16 },
}

/// Every pad the console reports. Polled by the input thread.
pub trait Pads: Send {
    /// Appends one sample per connected pad.
    fn poll(&mut self, out: &mut Vec<PadSample>);

    fn feedback(&mut self, slot: u8, feedback: &PadFeedback);

    /// Stops every pad's motors.
    fn release_all(&mut self);

    /// The latest motion sample of the pad in `slot`, after a [`Self::poll`]; `None` without
    /// a sensor.
    fn motion(&mut self, _slot: u8) -> Option<Motion> {
        None
    }

    /// The fingers on the screen now, replacing `out`'s contents. Nothing without a touch
    /// screen, or while the screen is off in the dock.
    fn touches(&mut self, out: &mut Vec<TouchPoint>) {
        out.clear();
    }
}

/// Where identity, settings, logs and bundles live.
pub trait Storage: Send + Sync {
    fn data_dir(&self) -> &Path;
}

/// What the system tells the title, polled once a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemEvent {
    /// The HOME menu opened over the title: release held input.
    FocusLost,
    FocusGained,
    /// The console is going to sleep: end the session cleanly.
    Suspend,
    Resume,
    /// The console was docked or undocked: the display's size and mode changed.
    OperationMode(OperationMode),
}

/// The title's life. [`Self::exit`] is the only way out: returning from `main` is a crash on
/// the console.
pub trait Lifecycle {
    fn hide_splash(&self);
    fn poll_events(&mut self, out: &mut Vec<SystemEvent>);
    fn exit(&self) -> !;

    /// The clocks the system runs the title at now, for the logs. `None` where they cannot be
    /// read: a dev box, or a console before firmware 8.0.0.
    fn clocks(&self) -> Option<Clocks> {
        None
    }

    /// Shows the system's keyboard over the title and blocks until the player closes it: the
    /// text they confirmed, or `None` when they cancelled or there is no such keyboard.
    fn keyboard(&mut self) -> Option<String> {
        None
    }
}

/// The CPU, GPU and memory clocks, in Hz.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Clocks {
    pub cpu_hz: u32,
    pub gpu_hz: u32,
    pub emc_hz: u32,
}

impl std::fmt::Display for Clocks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mhz = |hz: u32| f64::from(hz) / 1e6;
        write!(
            f,
            "gpu {:.1} cpu {:.1} emc {:.1} MHz",
            mhz(self.gpu_hz),
            mhz(self.cpu_hz),
            mhz(self.emc_hz)
        )
    }
}
