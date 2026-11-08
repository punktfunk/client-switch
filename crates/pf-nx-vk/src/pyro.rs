//! The `PyroWave` lane: the shared decoder (`pf_client_video::video_pyrowave`) on the one device,
//! publishing three-plane pictures to the presenter's slot.
//!
//! Intra-only: every frame decodes on its own, so a loss needs no re-anchor and a damaged
//! frame is local blur for that frame only.
//!
//! Each picture is waited on before it is published, so the `pyrowave[10s]` line can say what
//! the GPU took (plan D14: the X1 has to be measured against the wavelet decode). The presenter
//! would otherwise wait on the same turn itself.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use ash::vk::{self, Handle as _};
use pf_client_video::video_color::ColorDesc;
use pf_client_video::video_pyrowave::PyroWaveDecoder;
use pf_nx_app::core::media::{FeedFlags, MediaClock, VideoSink, VideoSinkCaps};
use pf_nx_app::platform::StreamSpec;

use crate::device::GpuDevice;
use crate::picture::{Picture, Planes, VideoSlot};

/// How often the lane logs where a picture's time went: the `stats:` line's cadence.
const TIMING_WINDOW: Duration = Duration::from_secs(10);

/// Opens `PyroWave` sinks on `dev` that publish to `slot`.
#[derive(Clone)]
pub struct PyroLane {
    dev: Arc<GpuDevice>,
    slot: Arc<VideoSlot>,
}

impl PyroLane {
    pub fn new(dev: Arc<GpuDevice>, slot: Arc<VideoSlot>) -> Self {
        Self { dev, slot }
    }

    /// Whether the device has the compute feature set `PyroWave` needs.
    pub fn available(&self) -> bool {
        self.dev.vkd.pyrowave_decode
    }

    pub fn open(&self, spec: &StreamSpec) -> Result<Box<dyn VideoSink>> {
        if !self.available() {
            bail!("this GPU lacks the PyroWave compute feature set");
        }
        // The bitstream carries no VUI; the colour contract is the handshake's.
        let color = ColorDesc {
            primaries: spec.color.primaries,
            transfer: spec.color.transfer,
            matrix: spec.color.matrix,
            full_range: spec.color.full_range != 0,
        };
        let ten_bit = spec.bit_depth >= 10;
        let decoder = PyroWaveDecoder::new(
            &self.dev.vkd,
            spec.width,
            spec.height,
            spec.shard_payload,
            spec.chroma444,
            color,
            ten_bit,
        )?;
        tracing::info!(
            "pyrowave: {}x{} {}-bit {}, shard payload {}",
            spec.width,
            spec.height,
            if ten_bit { 10 } else { 8 },
            if spec.chroma444 { "4:4:4" } else { "4:2:0" },
            spec.shard_payload
        );
        // The decoder's plane ring retires on a newest-wins hand-off, and all-intra frames leave
        // nothing for a queue to smooth: the desktop presenter makes the same call.
        self.slot.set_smoothing(0);
        Ok(Box::new(PyroSink {
            decoder: Mutex::new((decoder, Timing::new())),
            slot: self.slot.clone(),
            hud: spec.hud.clone(),
            e2e: spec.video_e2e.clone(),
            started: Instant::now(),
        }))
    }
}

/// One window of decode times, µs.
struct Timing {
    since: Instant,
    pictures: u32,
    bytes: u64,
    parse_us: u64,
    parse_max_us: u32,
    record_us: u64,
    record_max_us: u32,
    gpu_us: u64,
    gpu_max_us: u32,
}

impl Timing {
    fn new() -> Self {
        Self {
            since: Instant::now(),
            pictures: 0,
            bytes: 0,
            parse_us: 0,
            parse_max_us: 0,
            record_us: 0,
            record_max_us: 0,
            gpu_us: 0,
            gpu_max_us: 0,
        }
    }

    fn note(&mut self, bytes: usize, parse_us: u32, record_us: u32, gpu_us: u32) {
        self.pictures += 1;
        self.bytes += bytes as u64;
        self.parse_us += u64::from(parse_us);
        self.parse_max_us = self.parse_max_us.max(parse_us);
        self.record_us += u64::from(record_us);
        self.record_max_us = self.record_max_us.max(record_us);
        self.gpu_us += u64::from(gpu_us);
        self.gpu_max_us = self.gpu_max_us.max(gpu_us);
    }

    /// Logs and restarts the window once it is due, with the decoder's own stage times.
    fn flush(&mut self, decoder: &PyroWaveDecoder) {
        let elapsed = self.since.elapsed();
        if elapsed < TIMING_WINDOW {
            return;
        }
        let n = u64::from(self.pictures.max(1));
        let ms = |us: u64| us as f64 / 1_000.0;
        tracing::info!(
            "pyrowave[10s]: {} pictures, {:.1} Mb/s, parse {:.2}/{:.2} ms, record {:.2}/{:.2} ms, gpu {:.2}/{:.2} ms (mean/max)",
            self.pictures,
            self.bytes as f64 * 8.0 / elapsed.as_secs_f64() / 1e6,
            ms(self.parse_us / n),
            ms(u64::from(self.parse_max_us)),
            ms(self.record_us / n),
            ms(u64::from(self.record_max_us)),
            ms(self.gpu_us / n),
            ms(u64::from(self.gpu_max_us)),
        );
        for line in decoder.gpu_stage_report(true) {
            tracing::info!("pyrowave stage: {line}");
        }
        *self = Self::new();
    }
}

struct PyroSink {
    decoder: Mutex<(PyroWaveDecoder, Timing)>,
    slot: Arc<VideoSlot>,
    hud: Arc<punktfunk_core::hud::Stats>,
    e2e: pf_nx_app::platform::VideoE2e,
    started: Instant,
}

impl VideoSink for PyroSink {
    fn name(&self) -> &'static str {
        "PyroWave"
    }

    fn caps(&self) -> VideoSinkCaps {
        VideoSinkCaps {
            pts: true,
            partial_au: false,
            flush: false,
            intra_only: true,
        }
    }

    fn feed(&self, au: &[u8], _pts_ns: u64, flags: FeedFlags) -> Result<()> {
        let mut guard = self.decoder.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (decoder, timing) = &mut *guard;
        let Some(frame) = decoder.decode_frame(au, flags.chunk_aligned, flags.complete)? else {
            return Ok(());
        };
        decoder.wait_ready(&frame)?;
        let split = decoder.last_split();
        timing.note(au.len(), split.parse_us, split.record_us, split.gpu_us);
        timing.flush(decoder);
        let views = frame.views.map(vk::ImageView::from_raw);
        self.slot.publish(Picture {
            planes: Planes::Three(views),
            // The decoder leaves its planes in GENERAL; sampling from it needs no transition.
            layout: vk::ImageLayout::GENERAL,
            width: frame.width,
            height: frame.height,
            color: frame.color,
            depth: if frame.ten_bit { 10 } else { 8 },
            msb_packed: frame.ten_bit,
            uv_scale: [1.0, 1.0],
            decoded_at: Instant::now(),
            due: None,
            pts_ns: flags.host_pts_ns,
            decoded_ns: u64::try_from(punktfunk_core::client::now_realtime_ns()).unwrap_or(0),
            turn: Some(frame.turn),
            hud: Some(self.hud.clone()),
            e2e: Some(self.e2e.clone()),
            hold: Box::new(frame.hold),
        });
        Ok(())
    }

    fn clock(&self) -> Option<&dyn MediaClock> {
        Some(self)
    }
}

impl MediaClock for PyroSink {
    fn now_ns(&self) -> u64 {
        self.started.elapsed().as_nanos() as u64
    }
}
