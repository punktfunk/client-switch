//! The client's seams on a developer box, so the app runs against a real Vulkan device and real
//! sockets without a console. Linux gets the presenter offscreen on the system loader, the
//! console's video lane on libavcodec's software decoders in place of NVDEC, and the `PyroWave`
//! lane; elsewhere the UI rasters on the CPU and H.264/HEVC are received and counted, not decoded.

use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use pf_nx_app::core::caps::VideoCaps;
use pf_nx_app::core::media::{FeedFlags, MediaClock, VideoSink, VideoSinkCaps};
use pf_nx_app::platform::fake::{FakeLifecycle, FakePads};
use pf_nx_app::platform::{
    AudioOut, AudioPort, DeviceInfo, Display, Lifecycle, Platform, Storage, StreamSpec, SystemEvent, VideoDecoders,
};

/// How the host platform is asked to run.
#[derive(Debug)]
pub struct Options {
    pub data_dir: PathBuf,
    pub width: u32,
    pub height: u32,
    /// `pads.json`: a list of batches of `PadSample`-shaped objects, one batch per poll.
    pub pad_script: Option<PathBuf>,
}

impl Options {
    /// From the command line and the environment: `--data-dir DIR` or `PF_NX_DATA`,
    /// `PF_NX_SIZE=WxH` (the docked 1920x1080 by default), `PF_NX_PADS`.
    pub fn from_env() -> Self {
        Self::parse(std::env::args().skip(1), |key| std::env::var_os(key))
    }

    fn parse(mut args: impl Iterator<Item = String>, env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Self {
        let mut data_dir = env("PF_NX_DATA").map(PathBuf::from);
        while let Some(arg) = args.next() {
            if arg == "--data-dir" {
                data_dir = args.next().map(PathBuf::from);
            } else if let Some(dir) = arg.strip_prefix("--data-dir=") {
                data_dir = Some(dir.into());
            }
        }
        let size = env("PF_NX_SIZE").and_then(|s| s.into_string().ok());
        let (width, height) = size
            .as_deref()
            .and_then(|s| s.split_once('x'))
            .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
            .unwrap_or((1920, 1080));
        Self {
            data_dir: data_dir.unwrap_or_else(|| std::env::temp_dir().join("pf-nx-host")),
            width,
            height,
            pad_script: env("PF_NX_PADS").map(PathBuf::from),
        }
    }
}

struct Dir(PathBuf);

impl Storage for Dir {
    fn data_dir(&self) -> &Path {
        &self.0
    }
}

/// A process exit, which is the right ending off the console.
struct HostLifecycle(FakeLifecycle);

impl Lifecycle for HostLifecycle {
    fn hide_splash(&self) {}

    fn poll_events(&mut self, out: &mut Vec<SystemEvent>) {
        self.0.poll_events(out);
    }

    fn exit(&self) -> ! {
        std::process::exit(0)
    }
}

/// Receives H.264/HEVC and counts it, where no Vulkan device carries the console's video lane.
struct CountingSink {
    fed: Arc<AtomicU64>,
    started: std::time::Instant,
}

impl VideoSink for CountingSink {
    fn name(&self) -> &'static str {
        "counting (no decoder on this box)"
    }

    fn caps(&self) -> VideoSinkCaps {
        VideoSinkCaps {
            pts: true,
            partial_au: false,
            flush: false,
            intra_only: false,
        }
    }

    fn feed(&self, _au: &[u8], _pts_ns: u64, _flags: FeedFlags) -> Result<()> {
        self.fed.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn clock(&self) -> Option<&dyn MediaClock> {
        Some(self)
    }
}

impl MediaClock for CountingSink {
    fn now_ns(&self) -> u64 {
        self.started.elapsed().as_nanos() as u64
    }
}

struct HostVideo {
    #[cfg(all(target_os = "linux", feature = "pyrowave"))]
    pyro: Option<pf_nx_vk::PyroLane>,
    /// The console's H.264/HEVC lane on the presenter's device, decoding in software.
    #[cfg(target_os = "linux")]
    lane: Option<pf_nx_platform::video::NxVideo>,
    fed: Arc<AtomicU64>,
}

impl VideoDecoders for HostVideo {
    fn caps(&self) -> VideoCaps {
        VideoCaps {
            #[cfg(all(target_os = "linux", feature = "pyrowave"))]
            pyrowave: self.pyro.as_ref().is_some_and(pf_nx_vk::PyroLane::available),
            #[cfg(not(all(target_os = "linux", feature = "pyrowave")))]
            pyrowave: false,
            ..VideoCaps::SWITCH
        }
    }

    fn open(&self, spec: &StreamSpec) -> Result<Box<dyn VideoSink>> {
        #[cfg(all(target_os = "linux", feature = "pyrowave"))]
        if spec.codec == punktfunk_core::quic::CODEC_PYROWAVE {
            if let Some(pyro) = &self.pyro {
                return pyro.open(spec);
            }
        }
        #[cfg(target_os = "linux")]
        if let Some(lane) = &self.lane {
            return lane.open(spec);
        }
        let _ = spec;
        Ok(Box::new(CountingSink {
            fed: self.fed.clone(),
            started: std::time::Instant::now(),
        }))
    }
}

/// Every grain into `audio.wav` (16-bit PCM), paced like a real port.
struct WavAudio {
    dir: PathBuf,
}

impl AudioOut for WavAudio {
    fn open(&self, channels: u8, rate: u32) -> Result<Box<dyn AudioPort>> {
        let path = self.dir.join("audio.wav");
        let mut file = std::fs::File::create(&path).with_context(|| format!("create {}", path.display()))?;
        file.write_all(&wav_header(channels, rate, 0))?;
        Ok(Box::new(WavPort {
            file,
            channels,
            rate,
            bytes: 0,
        }))
    }
}

struct WavPort {
    file: std::fs::File,
    channels: u8,
    rate: u32,
    bytes: u32,
}

const GRAIN: usize = 256;

impl AudioPort for WavPort {
    fn grain_frames(&self) -> usize {
        GRAIN
    }

    fn write(&mut self, pcm: &[f32]) -> Result<()> {
        let mut out = Vec::with_capacity(pcm.len() * 2);
        for s in pcm {
            out.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
        }
        self.file.write_all(&out)?;
        self.bytes = self.bytes.saturating_add(out.len() as u32);
        std::thread::sleep(std::time::Duration::from_micros(
            GRAIN as u64 * 1_000_000 / u64::from(self.rate.max(1)),
        ));
        Ok(())
    }
}

impl Drop for WavPort {
    fn drop(&mut self) {
        let _ = self.file.seek(SeekFrom::Start(0));
        let _ = self.file.write_all(&wav_header(self.channels, self.rate, self.bytes));
    }
}

fn wav_header(channels: u8, rate: u32, data: u32) -> Vec<u8> {
    let block = u16::from(channels) * 2;
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(36 + data).to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes());
    h.extend_from_slice(&u16::from(channels).to_le_bytes());
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
    h.extend_from_slice(&block.to_le_bytes());
    h.extend_from_slice(&16u16.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data.to_le_bytes());
    h
}

/// Pads replaying `pads.json` when given, else none at all.
fn pads(script: Option<&Path>) -> Result<FakePads> {
    let pads = FakePads::default();
    if let Some(path) = script {
        let raw = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let batches: Vec<Vec<serde_json::Value>> = serde_json::from_slice(&raw).context("pads.json")?;
        let mut queue = pads.script.lock().expect("fresh pads");
        for batch in batches {
            queue.push_back(batch.iter().map(sample_from_json).collect());
        }
    }
    Ok(pads)
}

fn sample_from_json(v: &serde_json::Value) -> pf_nx_app::platform::PadSample {
    let n = |k: &str| v.get(k).and_then(serde_json::Value::as_i64).unwrap_or(0);
    pf_nx_app::platform::PadSample {
        slot: n("slot") as u8,
        buttons: n("buttons") as u32,
        lx: n("lx") as i16,
        ly: n("ly") as i16,
        rx: n("rx") as i16,
        ry: n("ry") as i16,
        lt: n("lt") as u8,
        rt: n("rt") as u8,
        ..Default::default()
    }
}

fn display(opts: &Options) -> Result<(Box<dyn Display>, HostVideo, String, String)> {
    let fed = Arc::new(AtomicU64::new(0));
    #[cfg(target_os = "linux")]
    {
        // SAFETY: loads the system Vulkan loader; the entry owns it for the process.
        match unsafe { ash::Entry::load() }
            .map_err(anyhow::Error::from)
            .and_then(|entry| {
                pf_nx_vk::GpuDevice::new(
                    entry,
                    pf_nx_vk::Target::Offscreen {
                        width: opts.width,
                        height: opts.height,
                    },
                )
            }) {
            Ok(dev) => {
                let dev = Arc::new(dev);
                let slot = Arc::new(pf_nx_vk::VideoSlot::default());
                let presenter = pf_nx_vk::Presenter::new(dev.clone(), slot.clone())?;
                let gpu = dev.name.clone();
                let ui = presenter.ui();
                let lane = Some(pf_nx_platform::video::NxVideo::gpu(dev.clone(), slot.clone()));
                #[cfg(feature = "pyrowave")]
                let pyro = Some(pf_nx_vk::PyroLane::new(dev, slot));
                #[cfg(not(feature = "pyrowave"))]
                drop((dev, slot));
                return Ok((
                    Box::new(presenter),
                    HostVideo {
                        #[cfg(feature = "pyrowave")]
                        pyro,
                        lane,
                        fed,
                    },
                    gpu,
                    ui,
                ));
            }
            Err(e) => tracing::warn!("no Vulkan here, rastering on the CPU: {e:#}"),
        }
    }
    Ok((
        Box::new(pf_nx_app::platform::fake::FakeDisplay::new(opts.width, opts.height)),
        HostVideo {
            #[cfg(all(target_os = "linux", feature = "pyrowave"))]
            pyro: None,
            #[cfg(target_os = "linux")]
            lane: None,
            fed,
        },
        "CPU raster".into(),
        format!("CPU raster {}x{}", opts.width, opts.height),
    ))
}

/// The host platform.
pub fn platform(opts: &Options) -> Result<Platform> {
    std::fs::create_dir_all(&opts.data_dir).with_context(|| format!("create {}", opts.data_dir.display()))?;
    let (display, video, gpu, ui) = display(opts)?;
    Ok(Platform {
        display,
        video: Arc::new(video),
        audio: Arc::new(WavAudio {
            dir: opts.data_dir.clone(),
        }),
        pads: Box::new(pads(opts.pad_script.as_deref())?),
        storage: Arc::new(Dir(opts.data_dir.clone())),
        lifecycle: Box::new(HostLifecycle(FakeLifecycle::default())),
        log: Some(Box::new(std::io::stderr())),
        device: DeviceInfo {
            platform: "host",
            model: std::env::consts::OS.into(),
            firmware: String::new(),
            gpu,
            ui,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_is_44_bytes_and_sized() {
        let h = wav_header(2, 48_000, 1000);
        assert_eq!(h.len(), 44);
        assert_eq!(u32::from_le_bytes(h[40..44].try_into().unwrap()), 1000);
    }

    #[test]
    fn the_data_dir_comes_from_the_command_line_first() {
        let env = |key: &str| (key == "PF_NX_DATA").then(|| "/from-env".into());
        let args = ["--data-dir", "/from-args"].map(String::from).into_iter();
        assert_eq!(Options::parse(args, env).data_dir, PathBuf::from("/from-args"));
        let opts = Options::parse(std::iter::empty(), env);
        assert_eq!(opts.data_dir, PathBuf::from("/from-env"));
        assert_eq!((opts.width, opts.height), (1920, 1080));
    }

    #[test]
    fn a_pad_script_replays_in_order() {
        let dir = std::env::temp_dir().join(format!("pf-host-pads-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pads.json");
        std::fs::write(&path, r#"[[{"slot":0,"buttons":4096}],[]]"#).unwrap();
        let mut p = pads(Some(&path)).unwrap();
        let mut out = Vec::new();
        pf_nx_app::platform::Pads::poll(&mut p, &mut out);
        assert_eq!(out[0].buttons, 4096);
    }
}
