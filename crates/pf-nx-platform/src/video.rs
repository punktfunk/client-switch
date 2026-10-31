//! H.264 and HEVC on NVDEC through libavcodec's `nvtegra` hwaccel (plan D6, §3.2). On a Linux dev
//! box the same lane runs on libavcodec's software decoders (`pf_nx_sys::av`), so the presenter
//! shows real pictures there.
//!
//! `feed` decodes on the session's video thread: send the access unit, then receive every
//! picture straight into the presenter's next staging buffer, which `commit` copies into a
//! plane set and publishes (D7). On the framebuffer display the pictures land in scratch planes
//! and are dropped, so the emulator still runs the hwaccel. A decoder error fails this feed,
//! which has the session ask for a keyframe, and the next feed starts from a flushed decoder.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Result};
use pf_nx_app::core::caps::VideoCaps;
use pf_nx_app::core::media::{FeedFlags, MediaClock, VideoSink, VideoSinkCaps};
use pf_nx_app::platform::{StreamSpec, VideoDecoders};
use pf_nx_sys::av;
use pf_nx_vk::{GpuDevice, Nv12Upload, StagingPlanes, VideoSlot};
use punktfunk_core::quic;

/// How often the lane logs where a picture's time went: the `stats:` line's cadence.
const TIMING_WINDOW: Duration = Duration::from_secs(10);

/// The decoder behind the lane, for the sink's name and the log.
#[cfg(target_os = "horizon")]
const DECODER: &str = "NVDEC";
#[cfg(not(target_os = "horizon"))]
const DECODER: &str = "libavcodec (software)";

pub struct NxVideo {
    /// The presenter's device and slot; `None` on the framebuffer display.
    gpu: Option<(Arc<GpuDevice>, Arc<VideoSlot>)>,
}

impl NxVideo {
    pub fn gpu(dev: Arc<GpuDevice>, slot: Arc<VideoSlot>) -> Self {
        Self { gpu: Some((dev, slot)) }
    }

    pub fn cpu() -> Self {
        Self { gpu: None }
    }
}

impl VideoDecoders for NxVideo {
    fn caps(&self) -> VideoCaps {
        VideoCaps::SWITCH
    }

    fn open(&self, spec: &StreamSpec) -> Result<Box<dyn VideoSink>> {
        let codec = match spec.codec {
            quic::CODEC_H264 => av::Codec::H264,
            quic::CODEC_HEVC => av::Codec::Hevc,
            other => bail!("{DECODER} here decodes H.264 and HEVC, not codec 0x{other:02x}"),
        };
        ensure!(
            spec.bit_depth <= 8 && !spec.chroma444,
            "{DECODER} here decodes 8-bit 4:2:0, not {}-bit{}",
            spec.bit_depth,
            if spec.chroma444 { " 4:4:4" } else { "" }
        );
        let decoder = av::Decoder::open(codec, spec.width, spec.height)?;
        let out = match &self.gpu {
            Some((dev, slot)) => Out::Gpu(Nv12Upload::new(dev.clone(), slot.clone(), spec)?),
            None => Out::scratch(spec),
        };
        tracing::info!("{DECODER}: {codec:?} {}x{}", spec.width, spec.height);
        let started = Instant::now();
        Ok(Box::new(NxSink {
            lane: Mutex::new(Lane {
                decoder,
                out,
                needs_flush: false,
                timing: Timing::new(),
                started,
                dues: VecDeque::with_capacity(DUES_KEPT + 1),
            }),
            started,
        }))
    }

    fn lane_name(&self, _codec: u8) -> &'static str {
        DECODER
    }
}

/// Where received pictures go.
enum Out {
    Gpu(Nv12Upload),
    /// Planes nothing reads, for the framebuffer display.
    Scratch {
        y: Vec<u8>,
        uv: Vec<u8>,
        stride: usize,
    },
}

impl Out {
    fn scratch(spec: &StreamSpec) -> Self {
        let stride = spec.width.next_multiple_of(2) as usize;
        let rows = spec.height.next_multiple_of(2) as usize;
        Self::Scratch {
            y: vec![0; stride * rows],
            uv: vec![0; stride * rows / 2],
            stride,
        }
    }

    fn stage(&mut self) -> Result<StagingPlanes<'_>> {
        match self {
            Self::Gpu(upload) => upload.stage(),
            Self::Scratch { y, uv, stride } => Ok(StagingPlanes {
                y,
                y_stride: *stride,
                uv,
                uv_stride: *stride,
            }),
        }
    }

    /// `false` when the picture was dropped because every plane set is on screen.
    fn commit(&mut self, meta: av::PictureMeta, due: Option<Instant>) -> Result<bool> {
        match self {
            Self::Gpu(upload) => upload.commit(meta.width, meta.height, meta.pts, due),
            Self::Scratch { .. } => Ok(true),
        }
    }
}

/// Due times kept for pictures not out of the decoder yet. The stream has no reordering, so one
/// is the rule; the rest bound a decoder that swallowed access units.
const DUES_KEPT: usize = 8;

struct Lane {
    decoder: av::Decoder,
    out: Out,
    /// The last feed failed; flush before the next one.
    needs_flush: bool,
    timing: Timing,
    /// The sink's clock origin, which the session's due times count from.
    started: Instant,
    /// `(host PTS, due ns)` per access unit fed, oldest first, until its picture comes out.
    dues: VecDeque<(u64, u64)>,
}

impl Lane {
    fn feed(&mut self, au: &[u8], pts: u64, due_ns: u64) -> Result<()> {
        if std::mem::take(&mut self.needs_flush) {
            self.decoder.flush();
            self.dues.clear();
        }
        self.dues.push_back((pts, due_ns));
        if self.dues.len() > DUES_KEPT {
            self.dues.pop_front();
        }
        let fed = self.decode(au, pts);
        self.needs_flush = fed.is_err();
        fed
    }

    fn decode(&mut self, au: &[u8], pts: u64) -> Result<()> {
        while self.decoder.send(au, pts)? == av::SendStatus::Again {
            if !self.receive()? {
                bail!("the decoder is full and has no picture to give back");
            }
        }
        while self.receive()? {}
        Ok(())
    }

    /// Receives one picture into the next staging planes; `false` when none is ready.
    fn receive(&mut self) -> Result<bool> {
        let waited = Instant::now();
        let planes = self.out.stage()?;
        let wait = waited.elapsed();
        let got = self
            .decoder
            .receive_into(planes.y, planes.y_stride, planes.uv, planes.uv_stride)?;
        let Some(meta) = got else { return Ok(false) };
        let due = self.due_of(meta.pts);
        let started = Instant::now();
        let shown = self.out.commit(meta, due)?;
        self.timing.note(&self.decoder.stats(), wait, started.elapsed(), shown);
        Ok(true)
    }

    /// The due time fed with the access unit at host PTS `pts`, dropping any older ones: their
    /// pictures never came out. `None` (shown at once) when it is not kept.
    fn due_of(&mut self, pts: u64) -> Option<Instant> {
        while let Some((fed, due_ns)) = self.dues.pop_front() {
            if fed == pts {
                return Some(self.started + Duration::from_nanos(due_ns));
            }
        }
        None
    }
}

/// Where the lane's time went over one [`TIMING_WINDOW`]: the wait for a free staging buffer,
/// the decoder's own time, the VIC transfer into the staging planes, and the upload's submit.
struct Timing {
    since: Instant,
    pictures: u32,
    dropped: u32,
    decode_us: u64,
    decode_max_us: u32,
    transfer_us: u64,
    transfer_max_us: u32,
    wait: Duration,
    wait_max: Duration,
    upload: Duration,
    in_flight_max: u32,
}

impl Timing {
    fn new() -> Self {
        Self {
            since: Instant::now(),
            pictures: 0,
            dropped: 0,
            decode_us: 0,
            decode_max_us: 0,
            transfer_us: 0,
            transfer_max_us: 0,
            wait: Duration::ZERO,
            wait_max: Duration::ZERO,
            upload: Duration::ZERO,
            in_flight_max: 0,
        }
    }

    fn note(&mut self, s: &av::Stats, wait: Duration, upload: Duration, shown: bool) {
        self.pictures += 1;
        self.dropped += u32::from(!shown);
        self.decode_us += u64::from(s.last_decode_us);
        self.decode_max_us = self.decode_max_us.max(s.last_decode_us);
        self.transfer_us += u64::from(s.last_transfer_us);
        self.transfer_max_us = self.transfer_max_us.max(s.last_transfer_us);
        self.wait += wait;
        self.wait_max = self.wait_max.max(wait);
        self.upload += upload;
        self.in_flight_max = self.in_flight_max.max(s.in_flight);
        if self.since.elapsed() < TIMING_WINDOW {
            return;
        }
        let n = u64::from(self.pictures);
        let ms = |us: u64| us as f64 / 1000.0;
        let per = |d: Duration| d.as_secs_f64() * 1000.0 / n as f64;
        tracing::info!(
            "nvdec[{}s]: {} pictures ({} dropped, {} decoded in all); staging wait mean {:.1}ms max {:.1}ms; decode mean {:.1}ms max {:.1}ms; transfer mean {:.1}ms max {:.1}ms; upload mean {:.2}ms; in flight max {}",
            TIMING_WINDOW.as_secs(),
            self.pictures,
            self.dropped,
            s.decoded,
            per(self.wait),
            self.wait_max.as_secs_f64() * 1000.0,
            ms(self.decode_us / n),
            ms(u64::from(self.decode_max_us)),
            ms(self.transfer_us / n),
            ms(u64::from(self.transfer_max_us)),
            per(self.upload),
            self.in_flight_max,
        );
        *self = Self::new();
    }
}

struct NxSink {
    lane: Mutex<Lane>,
    started: Instant,
}

impl NxSink {
    fn lane(&self) -> std::sync::MutexGuard<'_, Lane> {
        self.lane.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl VideoSink for NxSink {
    fn name(&self) -> &'static str {
        DECODER
    }

    fn caps(&self) -> VideoSinkCaps {
        VideoSinkCaps {
            pts: true,
            partial_au: false,
            flush: false,
            intra_only: false,
        }
    }

    /// `due_ns` is the session's pacing on this sink's clock: when the picture should reach the
    /// glass, which a smoothing slot holds it for.
    fn feed(&self, au: &[u8], due_ns: u64, flags: FeedFlags) -> Result<()> {
        self.lane().feed(au, flags.host_pts_ns, due_ns)
    }

    fn clock(&self) -> Option<&dyn MediaClock> {
        Some(self)
    }

    /// The decoder's own time. The VIC transfer and the upload are this lane's cost, not the
    /// decoder's, and charging them would hold the bitrate controller under what NVDEC sustains.
    fn decode_us(&self) -> Option<u32> {
        Some(self.lane().decoder.stats().last_decode_us)
    }
}

impl MediaClock for NxSink {
    fn now_ns(&self) -> u64 {
        self.started.elapsed().as_nanos() as u64
    }
}
