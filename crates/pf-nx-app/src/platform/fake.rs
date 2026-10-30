//! In-memory implementations of every seam, for tests and for a headless run. Each records
//! what the application asked of it so a test can assert on the calls.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;

use super::{
    AudioOut, AudioPort, Display, Fit, FrameReport, Lifecycle, PadFeedback, PadSample, Pads, Refresh, Rgba, Storage,
    StreamSpec, SystemEvent, VideoDecoders,
};
use crate::core::caps::VideoCaps;
use crate::core::media::{VideoSink, VideoSinkCaps};

/// A CPU raster surface standing in for the swapchain.
pub struct FakeDisplay {
    surface: skia_safe::Surface,
    refresh: Refresh,
    pub frames: u64,
    pub stream: Option<Fit>,
}

impl FakeDisplay {
    pub fn new(width: u32, height: u32) -> Self {
        let surface = skia_safe::surfaces::raster_n32_premul((width as i32, height as i32))
            .expect("raster surface of a positive size");
        Self {
            surface,
            refresh: Refresh(59_940),
            frames: 0,
            stream: None,
        }
    }
}

impl Display for FakeDisplay {
    fn size(&self) -> (u32, u32) {
        (self.surface.width() as u32, self.surface.height() as u32)
    }

    fn refresh(&self) -> Refresh {
        self.refresh
    }

    fn frame(&mut self, ui: &mut dyn FnMut(&skia_safe::Canvas)) -> Result<FrameReport> {
        let canvas = self.surface.canvas();
        canvas.clear(skia_safe::Color::BLACK);
        ui(canvas);
        self.frames += 1;
        Ok(FrameReport::default())
    }

    fn set_stream(&mut self, on: bool, fit: Fit) {
        self.stream = on.then_some(fit);
    }

    fn read_back(&mut self) -> Result<Option<Rgba>> {
        let (width, height) = self.size();
        let info = skia_safe::ImageInfo::new(
            (width as i32, height as i32),
            skia_safe::ColorType::RGBA8888,
            skia_safe::AlphaType::Unpremul,
            None,
        );
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        let ok = self.surface.read_pixels(&info, &mut pixels, width as usize * 4, (0, 0));
        Ok(ok.then_some(Rgba { width, height, pixels }))
    }
}

/// Every access unit a [`FakeSink`] was fed, newest last.
pub type FedLog = Arc<Mutex<Vec<(usize, u64)>>>;

/// Decoders that accept everything and remember what they were fed.
#[derive(Default)]
pub struct FakeVideo {
    pub caps: Option<VideoCaps>,
    pub opened: Mutex<Vec<StreamSpec>>,
    pub fed: FedLog,
}

impl VideoDecoders for FakeVideo {
    fn caps(&self) -> VideoCaps {
        self.caps.unwrap_or(VideoCaps::SWITCH)
    }

    fn open(&self, spec: &StreamSpec) -> Result<Box<dyn VideoSink>> {
        self.opened.lock().expect("fake video poisoned").push(spec.clone());
        Ok(Box::new(FakeSink {
            fed: self.fed.clone(),
            clock: std::time::Instant::now(),
        }))
    }

    fn lane_name(&self, _codec: u8) -> &'static str {
        "fake"
    }
}

/// A sink that takes whole access units with timestamps, like the console's lanes.
pub struct FakeSink {
    fed: FedLog,
    clock: std::time::Instant,
}

impl VideoSink for FakeSink {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn caps(&self) -> VideoSinkCaps {
        VideoSinkCaps {
            pts: true,
            partial_au: false,
            flush: true,
            intra_only: false,
        }
    }

    fn feed(&self, au: &[u8], pts_ns: u64, _flags: crate::core::media::FeedFlags) -> Result<()> {
        self.fed.lock().expect("fake sink poisoned").push((au.len(), pts_ns));
        Ok(())
    }

    fn clock(&self) -> Option<&dyn crate::core::media::MediaClock> {
        Some(self)
    }
}

impl crate::core::media::MediaClock for FakeSink {
    fn now_ns(&self) -> u64 {
        self.clock.elapsed().as_nanos() as u64
    }
}

/// Every grain an opened [`FakePort`] played.
pub type Grains = Arc<Mutex<Vec<Vec<f32>>>>;

#[derive(Default)]
pub struct FakeAudio {
    pub grains: Grains,
    pub opened: Mutex<Vec<(u8, u32)>>,
}

impl AudioOut for FakeAudio {
    fn open(&self, channels: u8, rate: u32) -> Result<Box<dyn AudioPort>> {
        self.opened.lock().expect("fake audio poisoned").push((channels, rate));
        Ok(Box::new(FakePort {
            channels: usize::from(channels),
            grains: self.grains.clone(),
        }))
    }
}

pub struct FakePort {
    channels: usize,
    grains: Grains,
}

impl AudioPort for FakePort {
    fn grain_frames(&self) -> usize {
        256
    }

    fn write(&mut self, pcm: &[f32]) -> Result<()> {
        anyhow::ensure!(pcm.len() == 256 * self.channels, "grain of {} samples", pcm.len());
        // A real port blocks for the grain's duration; so does this one, or the writer spins.
        std::thread::sleep(std::time::Duration::from_micros(5_333));
        self.grains.lock().expect("fake port poisoned").push(pcm.to_vec());
        Ok(())
    }
}

/// Pads that replay a script, one batch per poll, and record the feedback sent to them.
#[derive(Default)]
pub struct FakePads {
    pub script: Arc<Mutex<VecDeque<Vec<PadSample>>>>,
    pub feedback: Arc<Mutex<Vec<(u8, PadFeedback)>>>,
    pub released: Arc<Mutex<u32>>,
    /// The last batch, repeated while the script is empty: a held pad stays held.
    last: Vec<PadSample>,
}

impl Pads for FakePads {
    fn poll(&mut self, out: &mut Vec<PadSample>) {
        if let Some(batch) = self.script.lock().expect("fake pads poisoned").pop_front() {
            self.last = batch;
        }
        out.extend_from_slice(&self.last);
    }

    fn feedback(&mut self, slot: u8, feedback: &PadFeedback) {
        self.feedback
            .lock()
            .expect("fake pads poisoned")
            .push((slot, feedback.clone()));
    }

    fn release_all(&mut self) {
        *self.released.lock().expect("fake pads poisoned") += 1;
    }
}

/// A directory the test owns.
pub struct FakeStorage(pub PathBuf);

impl FakeStorage {
    /// A fresh directory under the system temp dir, unique per call.
    pub fn temp(tag: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pf-nx-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp data dir");
        Self(dir)
    }
}

impl Storage for FakeStorage {
    fn data_dir(&self) -> &Path {
        &self.0
    }
}

#[derive(Default)]
pub struct FakeLifecycle {
    pub events: Arc<Mutex<VecDeque<SystemEvent>>>,
    pub splash_hidden: Arc<Mutex<bool>>,
}

impl Lifecycle for FakeLifecycle {
    fn hide_splash(&self) {
        *self.splash_hidden.lock().expect("fake lifecycle poisoned") = true;
    }

    fn poll_events(&mut self, out: &mut Vec<SystemEvent>) {
        out.extend(self.events.lock().expect("fake lifecycle poisoned").drain(..));
    }

    fn exit(&self) -> ! {
        panic!("title exited");
    }
}
