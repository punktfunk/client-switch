//! H.264 and HEVC on NVDEC through libavcodec's `nvtegra` hwaccel (plan D6, §3.2). On a Linux dev
//! box the same lane runs on libavcodec's software decoders (`pf_nx_sys::av`), so the presenter
//! shows real pictures there.
//!
//! `feed` decodes on the session's video thread: send the access unit, then receive every
//! picture. By the VIC a picture lands in the presenter's next staging buffer, which `commit`
//! copies into a plane set and publishes (D7): through libavutil's own frame and the shim's copy,
//! or with [`NxVideo::straight`] written there by the VIC itself (WP5). In place
//! ([`NxVideo::in_place`]) NVDEC's own surface goes to the presenter, which samples it as it
//! lies; the lane takes that route once a picture's surface reads back as the VIC copied it, and
//! leaves it for good on any mismatch. The `nvdec[10s]` line names the route and NVDEC's clock.
//! On the framebuffer display the pictures land in scratch planes and are dropped, so the
//! emulator still runs the hwaccel. A decoder error fails this feed, which has the session ask
//! for a keyframe, and the next feed starts from a flushed decoder.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, ensure, Result};
use pf_nx_app::core::caps::VideoCaps;
use pf_nx_app::core::media::{FeedFlags, MediaClock, VideoSink, VideoSinkCaps};
use pf_nx_app::platform::{StreamSpec, VideoDecoders};
use pf_nx_sys::av;
use pf_nx_vk::{Check, GpuDevice, InPlace, Nv12Upload, StagingPlanes, TiledSurface, VideoSlot};
use punktfunk_core::quic;

/// How often the lane logs where a picture's time went: the `stats:` line's cadence.
const TIMING_WINDOW: Duration = Duration::from_secs(10);
/// While the in-place route is checked, every this-many-th picture is checked: half a second at
/// 60 Hz, so a flat start costs little.
const CHECK_EVERY: u32 = 30;

/// The decoder behind the lane, for the sink's name and the log.
#[cfg(target_os = "horizon")]
const DECODER: &str = "NVDEC";
#[cfg(not(target_os = "horizon"))]
const DECODER: &str = "libavcodec (software)";

pub struct NxVideo {
    /// The presenter's device and slot; `None` on the framebuffer display.
    gpu: Option<(Arc<GpuDevice>, Arc<VideoSlot>)>,
    /// The VIC writes into the staging buffer itself. Off by default: on the console it neither
    /// lowered the transfer plus copy nor the end-to-end time (WP5, 2026-11-01).
    straight: bool,
    /// Pictures go to the presenter as NVDEC left them, once one checks out.
    in_place: bool,
    /// Whether NVDEC holds its top clock, asked at each access unit.
    top_clock: fn() -> bool,
}

impl NxVideo {
    pub fn gpu(dev: Arc<GpuDevice>, slot: Arc<VideoSlot>) -> Self {
        Self {
            gpu: Some((dev, slot)),
            straight: false,
            in_place: false,
            top_clock: || false,
        }
    }

    pub fn cpu() -> Self {
        Self {
            gpu: None,
            straight: false,
            in_place: false,
            top_clock: || false,
        }
    }

    /// Has the VIC write straight into the staging buffer where it can.
    pub fn straight(mut self, on: bool) -> Self {
        self.straight = on;
        self
    }

    /// Has the presenter sample NVDEC's surfaces where they lie, once one reads back right.
    /// The straight route keeps the VIC.
    pub fn in_place(mut self, on: bool) -> Self {
        self.in_place = on;
        self
    }

    /// Holds NVDEC at its top clock while `when` says so, asked at each access unit.
    pub fn top_clock(mut self, when: fn() -> bool) -> Self {
        self.top_clock = when;
        self
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
        let decoder = av::Decoder::open(codec, spec.width, spec.height, self.straight)?;
        let out = match &self.gpu {
            Some((dev, slot)) => Out::Gpu(Nv12Upload::new(dev.clone(), slot.clone(), spec)?),
            None => Out::scratch(spec),
        };
        let route = match &self.gpu {
            Some((dev, slot)) if self.in_place && !self.straight => {
                match InPlace::new(dev.clone(), slot.clone(), spec) {
                    Ok(lane) => Route::Checking(lane, 0),
                    Err(e) => {
                        tracing::info!("in place: off for this stream: {e:#}");
                        Route::Vic
                    }
                }
            }
            _ => Route::Vic,
        };
        tracing::info!(
            "{DECODER}: {codec:?} {}x{}, {}",
            spec.width,
            spec.height,
            match (&route, self.straight) {
                (Route::Checking(..), _) => "VIC then copy until a picture checks out in place",
                (_, true) => "straight VIC route asked for",
                _ => "VIC then copy",
            }
        );
        let started = Instant::now();
        Ok(Box::new(NxSink {
            lane: Mutex::new(Lane {
                route,
                out,
                decoder,
                top_clock: self.top_clock,
                needs_flush: false,
                #[cfg(target_os = "horizon")]
                raised: false,
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

    /// The GPU time of the last staging copy into a plane set, where the GPU can time it.
    fn copy_gpu_us(&self) -> Option<u32> {
        match self {
            Self::Gpu(upload) => upload.copy_gpu_us(),
            Self::Scratch { .. } => None,
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

/// How the lane's pictures reach the presenter.
enum Route {
    /// By the VIC into the staging planes.
    Vic,
    /// By the VIC, while every [`CHECK_EVERY`]th picture is checked in place; pictures so far.
    Checking(InPlace, u32),
    /// As NVDEC left them.
    InPlace(InPlace),
}

/// Fields drop in order: the route's surface imports go before the decoder frees its surfaces.
struct Lane {
    route: Route,
    out: Out,
    decoder: av::Decoder,
    top_clock: fn() -> bool,
    /// The last feed failed; flush before the next one.
    needs_flush: bool,
    /// The thread that feeds the lane is the decode thread: its first access unit raises it.
    #[cfg(target_os = "horizon")]
    raised: bool,
    timing: Timing,
    /// The sink's clock origin, which the session's due times count from.
    started: Instant,
    /// `(host PTS, due ns)` per access unit fed, oldest first, until its picture comes out.
    dues: VecDeque<(u64, u64)>,
}

impl Lane {
    fn feed(&mut self, au: &[u8], pts: u64, due_ns: u64) -> Result<()> {
        #[cfg(target_os = "horizon")]
        if !std::mem::replace(&mut self.raised, true) {
            crate::console::threads::raise(crate::console::threads::HOT);
        }
        self.decoder.set_top_clock((self.top_clock)());
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

    /// Receives one picture by the lane's route; `false` when none is ready.
    fn receive(&mut self) -> Result<bool> {
        if matches!(self.route, Route::InPlace(_)) {
            return self.receive_in_place();
        }
        let waited = Instant::now();
        let planes = self.out.stage()?;
        let wait = waited.elapsed();
        let check = matches!(self.route, Route::Checking(_, n) if n % CHECK_EVERY == 0);
        let got = if check {
            self.decoder
                .receive_kept_into(planes.y, planes.y_stride, planes.uv, planes.uv_stride)?
        } else {
            self.decoder
                .receive_into(planes.y, planes.y_stride, planes.uv, planes.uv_stride)?
                .map(|meta| (meta, None))
        };
        let Some((meta, kept)) = got else { return Ok(false) };
        if let Route::Checking(_, n) = &mut self.route {
            *n += 1;
        }
        if check {
            settle(&mut self.route, kept.as_ref(), &planes);
        }
        let due = self.due_of(meta.pts);
        self.timing.note_due(due);
        let started = Instant::now();
        let shown = self.out.commit(meta, due)?;
        let upload = started.elapsed();
        self.timing
            .note(&self.decoder.stats(), wait, upload, self.out.copy_gpu_us(), shown);
        Ok(true)
    }

    /// Receives one picture as NVDEC left it and publishes it; `false` when none is ready. A
    /// surface the presenter cannot sample, or a failed import, puts the lane on the VIC.
    fn receive_in_place(&mut self) -> Result<bool> {
        let surface = match self.decoder.receive_surface() {
            Ok(Some(surface)) => surface,
            Ok(None) => return Ok(false),
            Err(e) if e.is::<av::NotInPlace>() => {
                tracing::warn!("in place: back to the VIC: {e:#}");
                self.route = Route::Vic;
                return Ok(true);
            }
            Err(e) => return Err(e),
        };
        let due = self.due_of(surface.pts());
        self.timing.note_due(due);
        let Route::InPlace(lane) = &mut self.route else {
            unreachable!("only the in-place route receives surfaces")
        };
        let started = Instant::now();
        let published = lane.publish(&tiled(&surface), surface.pts(), due, Box::new(surface));
        let upload = started.elapsed();
        if let Err(e) = &published {
            tracing::warn!("in place: back to the VIC: {e:#}");
            self.route = Route::Vic;
        }
        self.timing
            .note(&self.decoder.stats(), Duration::ZERO, upload, None, published.is_ok());
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

/// The surface as the presenter's in-place lane reads it.
fn tiled(s: &av::Surface) -> TiledSurface {
    TiledSurface {
        base: s.base(),
        size: s.size(),
        pitch: s.pitch(),
        chroma_offset: s.chroma_offset(),
        width: s.width(),
        height: s.height(),
        pool: s.pool(),
    }
}

/// Moves a checking route on from the check of `kept`, the surface of the picture the VIC has
/// just written into `planes`: in place on a match, back to the VIC for good on a mismatch or a
/// failure, unchanged on a picture too flat to tell.
fn settle(route: &mut Route, kept: Option<&av::Surface>, planes: &StagingPlanes<'_>) {
    let Route::Checking(lane, n) = route else { return };
    let verdict = match kept {
        Some(s) => lane.check(
            &tiled(s),
            &planes.y[..],
            planes.y_stride,
            &planes.uv[..],
            planes.uv_stride,
        ),
        None => Err(anyhow!("{}", av::NotInPlace)),
    };
    let n = *n;
    match verdict {
        Ok(Check::Flat) => {}
        Ok(Check::Match { atoms, differ }) => {
            tracing::info!(
                "in place: picture {n} read back as the VIC wrote it ({differ} of {atoms} atoms off); pictures now go in place"
            );
            if let Route::Checking(lane, _) = std::mem::replace(route, Route::Vic) {
                *route = Route::InPlace(lane);
            }
        }
        Ok(Check::Differ { atoms, differ }) => {
            tracing::warn!(
                "in place: {differ} of {atoms} atoms of picture {n} read back otherwise; staying on the VIC"
            );
            *route = Route::Vic;
        }
        Err(e) => {
            tracing::warn!("in place: the check of picture {n} failed, staying on the VIC: {e:#}");
            *route = Route::Vic;
        }
    }
}

/// Where the lane's time went over one [`TIMING_WINDOW`]: the wait for a free staging buffer,
/// the decoder's own time and clock, the transfer to NV12, the shim's copy out of libavutil's
/// frame (none when the transfer wrote the staging planes itself), the upload's submit, and the
/// staging copy's time on the GPU.
struct Timing {
    since: Instant,
    pictures: u32,
    dropped: u32,
    decode_us: u64,
    decode_max_us: u32,
    transfer_us: u64,
    transfer_max_us: u32,
    copy_us: u64,
    copy_max_us: u32,
    wait: Duration,
    wait_max: Duration,
    upload: Duration,
    gpu_copy_us: u64,
    gpu_copies: u32,
    in_flight_max: u32,
    /// How the last picture reached the staging planes; `None` before the first.
    route: Option<av::Route>,
    /// NVDEC's clock after the last picture, kHz.
    nvdec_khz: u32,
    /// Pictures whose host PTS found no due time: they are shown at once, whatever the budget.
    undue: u32,
    /// The due time less the moment the picture came out of the decoder, µs, over the pictures
    /// that had one. Negative is already late.
    slack_us: i64,
    slack_min_us: i64,
    slack_max_us: i64,
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
            copy_us: 0,
            copy_max_us: 0,
            wait: Duration::ZERO,
            wait_max: Duration::ZERO,
            upload: Duration::ZERO,
            gpu_copy_us: 0,
            gpu_copies: 0,
            in_flight_max: 0,
            route: None,
            nvdec_khz: 0,
            undue: 0,
            slack_us: 0,
            slack_min_us: i64::MAX,
            slack_max_us: i64::MIN,
        }
    }

    /// Whether the picture out now kept its due time, and how far ahead of it.
    fn note_due(&mut self, due: Option<Instant>) {
        let Some(due) = due else {
            self.undue += 1;
            return;
        };
        let now = Instant::now();
        let us = |d: Duration| i64::try_from(d.as_micros()).unwrap_or(i64::MAX);
        let slack = if due >= now { us(due - now) } else { -us(now - due) };
        self.slack_us += slack;
        self.slack_min_us = self.slack_min_us.min(slack);
        self.slack_max_us = self.slack_max_us.max(slack);
    }

    /// `"due slack mean … min … max … ms, N without a due"` over this window's pictures.
    fn due_summary(&self) -> String {
        let timed = i64::from(self.pictures.saturating_sub(self.undue));
        let ms = |us: i64| us as f64 / 1000.0;
        if timed == 0 {
            return format!("no due times, {} without", self.undue);
        }
        format!(
            "due slack mean {:.1}ms min {:.1}ms max {:.1}ms, {} without a due",
            ms(self.slack_us / timed),
            ms(self.slack_min_us),
            ms(self.slack_max_us),
            self.undue
        )
    }

    fn note(&mut self, s: &av::Stats, wait: Duration, upload: Duration, gpu_copy_us: Option<u32>, shown: bool) {
        // The first route, and any change, at once: a change mid-stream is the straight route
        // falling back.
        if self.route != Some(s.route) {
            tracing::info!(
                "nvdec: route {}{}{}",
                s.route.name(),
                self.route.map_or_else(String::new, |r| format!(" (was {})", r.name())),
                if s.direct_err == 0 {
                    String::new()
                } else {
                    format!("; the straight route failed with {}", s.direct_err)
                }
            );
        }
        self.route = Some(s.route);
        self.nvdec_khz = s.nvdec_khz;
        self.copy_us += u64::from(s.last_copy_us);
        self.copy_max_us = self.copy_max_us.max(s.last_copy_us);
        if let Some(us) = gpu_copy_us {
            self.gpu_copy_us += u64::from(us);
            self.gpu_copies += 1;
        }
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
        let gpu_copy = self
            .gpu_copy_us
            .checked_div(u64::from(self.gpu_copies))
            .map_or_else(|| "-".to_string(), |us| format!("{:.2}", us as f64 / 1000.0));
        tracing::info!(
            "nvdec[{}s]: {} pictures ({} dropped, {} decoded in all); staging wait mean {:.1}ms max {:.1}ms; decode mean {:.1}ms max {:.1}ms at {:.1}MHz; transfer mean {:.1}ms max {:.1}ms; copy mean {:.1}ms max {:.1}ms; upload mean {:.2}ms; gpu copy mean {gpu_copy}ms; in flight max {}; route {}; {}",
            TIMING_WINDOW.as_secs(),
            self.pictures,
            self.dropped,
            s.decoded,
            per(self.wait),
            self.wait_max.as_secs_f64() * 1000.0,
            ms(self.decode_us / n),
            ms(u64::from(self.decode_max_us)),
            f64::from(self.nvdec_khz) / 1000.0,
            ms(self.transfer_us / n),
            ms(u64::from(self.transfer_max_us)),
            ms(self.copy_us / n),
            ms(u64::from(self.copy_max_us)),
            per(self.upload),
            self.in_flight_max,
            self.route.map_or("-", av::Route::name),
            self.due_summary(),
        );
        let route = self.route;
        *self = Self::new();
        self.route = route;
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
