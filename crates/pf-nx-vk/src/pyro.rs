//! The `PyroWave` lane: the shared decoder (`pf_client_video::video_pyrowave`) on the one device,
//! publishing three-plane pictures to the presenter's slot.
//!
//! Intra-only: every frame decodes on its own, so a loss needs no re-anchor and a damaged
//! frame is local blur for that frame only.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{bail, Result};
use ash::vk::{self, Handle as _};
use pf_client_video::video_color::ColorDesc;
use pf_client_video::video_pyrowave::PyroWaveDecoder;
use pf_nx_app::core::media::{FeedFlags, MediaClock, VideoSink, VideoSinkCaps};
use pf_nx_app::platform::StreamSpec;

use crate::device::GpuDevice;
use crate::picture::{Picture, Planes, VideoSlot};

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
            decoder: Mutex::new(decoder),
            slot: self.slot.clone(),
            hud: spec.hud.clone(),
            started: Instant::now(),
        }))
    }
}

struct PyroSink {
    decoder: Mutex<PyroWaveDecoder>,
    slot: Arc<VideoSlot>,
    hud: Arc<punktfunk_core::hud::Stats>,
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
        let mut decoder = self.decoder.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(frame) = decoder.decode_frame(au, flags.chunk_aligned, flags.complete)? else {
            return Ok(());
        };
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
