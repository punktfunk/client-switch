//! The console's audio route: decoded PCM into a ring, a writer thread draining it into the
//! platform's [`AudioPort`] one grain at a time.
//!
//! The port blocks for a grain and nothing rate-matches it to the host, so depth is steered by
//! punktfunk-core's shared [`JitterPolicy`], the same one every client's ring runs.
//!
//! Every packet is also placed against the picture on the glass with core's [`AvSync`]: where
//! the audio will play in the host's capture clock, next to where the video is. That only
//! measures (plan WP5): the offset goes to the stats line and the `audio[10s]` line, and the
//! ring is not steered by it until a long session shows it drift. The same line splits the
//! underruns by whether the audio before them was silent, and counts the host's pauses.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use punktfunk_core::audio::{crossfade_drop, crossfade_insert, AvSync, AvSyncObservation, JitterPolicy, JitterTuning};
use punktfunk_core::client::NativeClient;

use crate::core::media::{AudioFormat, AudioSink, Samples};
use crate::platform::AudioPort;

/// No base target: the policy's floor (one 5.33 ms `AudioOut` grain plus a packet) is the depth,
/// and only underruns grow it. The ceilings and the de-prime stay WASAPI's, so a Wi-Fi link still
/// has room to grow; a clean link never reaches them.
const TUNING: JitterTuning = JitterTuning {
    base_target_ms: 0,
    headroom_ms: 15,
    ..JitterTuning::WASAPI
};

fn playout(channels: u8) -> punktfunk_core::audio::AudioPlayout {
    punktfunk_core::audio::AudioPlayout {
        tuning: TUNING,
        format: punktfunk_core::audio::plane::PlaneFormat::from_welcome(
            punktfunk_core::quic::AUDIO_CODEC_OPUS,
            crate::session::audio::SAMPLE_RATE,
            16,
            channels,
            0,
            0,
        ),
    }
}

/// How often the route logs its window.
const WINDOW: Duration = Duration::from_secs(10);
/// A packet this far below full scale is silence, so an underrun after it cannot be heard:
/// −50 dBFS.
const SILENT_PEAK: f32 = 0.003_16;
/// No packet for this long is the host pausing, not jitter.
const PAUSE: Duration = Duration::from_millis(100);

/// Core's cells the route reads and writes: the clock skew and the video's capture-to-glass
/// time in, the smoothed A/V offset and the ring's depth out (the stats line shows both).
#[derive(Clone, Default)]
pub struct SyncCells {
    pub clock_offset_ns: Arc<AtomicI64>,
    pub video_e2e_ns: Arc<AtomicU64>,
    pub av_offset_ms: Arc<AtomicI64>,
    pub buffer_ms: Arc<AtomicU32>,
}

impl SyncCells {
    pub fn of(client: &NativeClient) -> Self {
        Self {
            clock_offset_ns: client.clock_offset_shared(),
            video_e2e_ns: client.video_e2e_shared(),
            av_offset_ms: client.audio_av_offset_shared(),
            buffer_ms: client.audio_buffer_ms_shared(),
        }
    }
}

struct Ring {
    samples: VecDeque<f32>,
    policy: JitterPolicy,
    depth_ms: u32,
    sync: AvSync,
    /// The peak of the last packet fed, which tells a silent underrun from an audible one.
    last_peak: f32,
    last_fed: Option<Instant>,
    window: Window,
}

/// One [`WINDOW`] of what the route did, filled by both threads under the ring's lock.
#[derive(Default)]
struct Window {
    grains: u32,
    /// Grains of silence while the ring primed or re-primed: not underruns.
    priming: u32,
    silent_underruns: u32,
    audible_underruns: u32,
    sheds: u32,
    inserts: u32,
    depth_min_ms: Option<u32>,
    depth_max_ms: u32,
    packets: u32,
    pauses: u32,
    longest_gap: Duration,
    /// Unsmoothed A/V offsets, ms: positive is audio late.
    av_sum_ms: i64,
    av_n: u32,
    av_min_ms: Option<i64>,
    av_max_ms: Option<i64>,
}

struct Shared {
    ring: Mutex<Ring>,
    fed: Condvar,
    stop: AtomicBool,
    underruns: AtomicU64,
    /// The port's queue after the last write, in frames: audio that plays before anything fed now.
    port_queued: AtomicUsize,
    cells: SyncCells,
}

/// An [`AudioSink`] over one open port, plus the thread that plays it.
pub struct AudioOutSink {
    shared: Arc<Shared>,
    channels: u8,
    writer: Option<std::thread::JoinHandle<()>>,
}

impl AudioOutSink {
    pub fn start(port: Box<dyn AudioPort>, channels: u8, cells: SyncCells) -> Result<Self> {
        let rate = crate::session::audio::SAMPLE_RATE;
        let shared = Arc::new(Shared {
            ring: Mutex::new(Ring {
                samples: VecDeque::with_capacity(rate as usize * usize::from(channels) / 5),
                policy: playout(channels).jitter(),
                depth_ms: 0,
                sync: playout(channels).sync(),
                last_peak: 0.0,
                last_fed: None,
                window: Window::default(),
            }),
            fed: Condvar::new(),
            stop: AtomicBool::new(false),
            underruns: AtomicU64::new(0),
            port_queued: AtomicUsize::new(0),
            cells,
        });
        let thread_shared = shared.clone();
        let writer = std::thread::Builder::new()
            .name("pf-audio-out".into())
            .spawn(move || write_loop(&thread_shared, port, usize::from(channels)))
            .context("spawn audio writer")?;
        crate::platform::thread_spawned(&writer);
        Ok(Self {
            shared,
            channels,
            writer: Some(writer),
        })
    }

    pub fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Relaxed)
    }
}

impl Drop for AudioOutSink {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.fed.notify_all();
        if let Some(writer) = self.writer.take() {
            crate::services::join::join_with_timeout(
                writer,
                crate::services::join::SHUTDOWN_JOIN_TIMEOUT,
                "audio-out",
                || (),
            );
        }
    }
}

impl AudioSink for AudioOutSink {
    fn name(&self) -> &'static str {
        "AudioOut"
    }

    fn format(&self) -> AudioFormat {
        AudioFormat::PcmF32 {
            channels: self.channels,
            sample_rate: crate::session::audio::SAMPLE_RATE,
        }
    }

    fn feed(&self, samples: Samples<'_>, host_pts_ns: u64) -> Result<()> {
        let Samples::F32(pcm) = samples;
        let peak = pcm.iter().fold(0f32, |m, s| m.max(s.abs()));
        let now = Instant::now();
        let shared = &self.shared;
        let mut ring = shared.ring.lock().expect("audio ring poisoned");
        let r = &mut *ring;
        if let Some(gap) = r.last_fed.map(|t| now - t) {
            r.window.pauses += u32::from(gap >= PAUSE);
            r.window.longest_gap = r.window.longest_gap.max(gap);
        }
        r.last_fed = Some(now);
        r.last_peak = peak;
        r.window.packets += 1;
        // Everything that plays before this packet: the ring, and the port's queue.
        let ahead = r.samples.len() + shared.port_queued.load(Ordering::Relaxed) * usize::from(self.channels);
        let cells = &shared.cells;
        let video_e2e_ns = cells.video_e2e_ns.load(Ordering::Relaxed);
        let o = AvSyncObservation {
            pts_ns: host_pts_ns,
            now_local_ns: punktfunk_core::client::now_realtime_ns(),
            clock_offset_ns: cells.clock_offset_ns.load(Ordering::Relaxed),
            buffered_ahead: ahead,
            // What audout adds past its queue is not known here: a constant, so drift still shows.
            output_latency_ns: 0,
            video_e2e_ns: (video_e2e_ns > 0).then_some(video_e2e_ns),
        };
        if let Some(ms) = raw_offset_ms(&o, self.channels) {
            let w = &mut r.window;
            w.av_sum_ms += ms;
            w.av_n += 1;
            w.av_min_ms = Some(w.av_min_ms.map_or(ms, |m| m.min(ms)));
            w.av_max_ms = Some(w.av_max_ms.map_or(ms, |m| m.max(ms)));
        }
        r.sync.observe(o);
        cells
            .av_offset_ms
            .store(i64::from(r.sync.offset_ms()), Ordering::Relaxed);
        r.samples.extend(pcm.iter().copied());
        drop(ring);
        shared.fed.notify_one();
        Ok(())
    }

    fn depth_ms(&self) -> Option<i64> {
        Some(i64::from(
            self.shared.ring.lock().expect("audio ring poisoned").depth_ms,
        ))
    }

    fn underruns(&self) -> Option<u64> {
        Some(self.shared.underruns.load(Ordering::Relaxed))
    }
}

/// Plays grains until stopped. A grain the ring cannot fill plays the policy's silence or the
/// short read padded with zeros, so the port's clock never stalls.
fn write_loop(shared: &Shared, mut port: Box<dyn AudioPort>, channels: usize) {
    let want = port.grain_frames() * channels;
    let mut grain = vec![0f32; want];
    let mut since = Instant::now();
    while !shared.stop.load(Ordering::Relaxed) {
        let window = {
            let mut ring = shared.ring.lock().expect("audio ring poisoned");
            if ring.samples.is_empty() && !ring.policy.is_primed() {
                // Nothing to play yet: wait for the decoder rather than spinning on silence.
                ring = shared
                    .fed
                    .wait_timeout(ring, Duration::from_millis(20))
                    .expect("audio ring poisoned")
                    .0;
            }
            fill(&mut ring, &mut grain, &shared.underruns);
            shared.cells.buffer_ms.store(ring.depth_ms, Ordering::Relaxed);
            (since.elapsed() >= WINDOW).then(|| {
                let (target_ms, offset_ms) = (ring.policy.target_ms(), ring.sync.offset_ms());
                (std::mem::take(&mut ring.window), target_ms, offset_ms)
            })
        };
        if let Err(e) = port.write(&grain) {
            tracing::warn!("audio out: {e:#}");
            std::thread::sleep(Duration::from_millis(5));
        }
        shared.port_queued.store(port.queued_frames(), Ordering::Relaxed);
        // Off the lock and after the grain is queued, so the port has its queue to play.
        if let Some((w, target_ms, offset_ms)) = window {
            log_window(&w, since.elapsed(), target_ms, offset_ms, port.queued_frames());
            since = Instant::now();
        }
    }
}

/// Where the audio in `o` will play against its picture, in ms (positive is audio late): the
/// arithmetic of [`AvSync::observe`] without the smoothing, for the window's spread. `None`
/// before a picture reached the glass.
fn raw_offset_ms(o: &AvSyncObservation, channels: u8) -> Option<i64> {
    let video = i128::from(o.video_e2e_ns?);
    let per_ms = i128::from(crate::session::audio::SAMPLE_RATE / 1000) * i128::from(channels.max(1));
    let buffered_ns = o.buffered_ahead as i128 * 1_000_000 / per_ms;
    let audio = o.now_local_ns + buffered_ns + i128::from(o.clock_offset_ns) - i128::from(o.pts_ns);
    i64::try_from((audio - video) / 1_000_000).ok()
}

fn log_window(w: &Window, took: Duration, target_ms: u32, offset_ms: i32, queued_frames: usize) {
    let av = match (w.av_min_ms, w.av_max_ms) {
        (Some(min), Some(max)) => format!(
            "a/v {offset_ms:+} ms smoothed, {:+} mean, {min:+} to {max:+} over {} packets",
            w.av_sum_ms / i64::from(w.av_n.max(1)),
            w.av_n
        ),
        _ => "a/v not measured, no picture on the glass".to_string(),
    };
    tracing::info!(
        "audio[{:.0}s]: {} grains, {} priming; underruns {} silent, {} audible; {} sheds, {} inserts; depth {}-{} ms, target {target_ms} ms, device queue {} ms; {} packets, {} pauses over {} ms, longest gap {} ms; {av}",
        took.as_secs_f64(),
        w.grains,
        w.priming,
        w.silent_underruns,
        w.audible_underruns,
        w.sheds,
        w.inserts,
        w.depth_min_ms.unwrap_or(0),
        w.depth_max_ms,
        queued_frames / (crate::session::audio::SAMPLE_RATE / 1000) as usize,
        w.packets,
        w.pauses,
        PAUSE.as_millis(),
        w.longest_gap.as_millis(),
    );
}

/// One policy step and one read into `grain`.
fn fill(ring: &mut Ring, grain: &mut [f32], underruns: &AtomicU64) {
    let want = grain.len();
    let step = ring.policy.step(ring.samples.len(), want);
    ring.window.grains += 1;
    ring.window.sheds += u32::from(step.drop_front > 0);
    ring.window.inserts += u32::from(step.insert_front > 0);
    if step.drop_front > 0 {
        crossfade_drop(&mut ring.samples, step.drop_front, step.crossfade);
    }
    if step.insert_front > 0 {
        crossfade_insert(&mut ring.samples, step.insert_front, step.crossfade);
    }
    if step.silence {
        grain.fill(0.0);
        ring.window.priming += 1;
        ring.policy.note_read(false);
    } else {
        let take = want.min(ring.samples.len());
        for (dst, src) in grain.iter_mut().zip(ring.samples.drain(..take)) {
            *dst = src;
        }
        grain[take..].fill(0.0);
        let short = take < want;
        if short {
            underruns.fetch_add(1, Ordering::Relaxed);
            if ring.last_peak < SILENT_PEAK {
                ring.window.silent_underruns += 1;
            } else {
                ring.window.audible_underruns += 1;
            }
        }
        ring.policy.note_read(short);
    }
    ring.depth_ms = ring.policy.depth_ms(ring.samples.len());
    let (depth, w) = (ring.depth_ms, &mut ring.window);
    w.depth_min_ms = Some(w.depth_min_ms.map_or(depth, |m| m.min(depth)));
    w.depth_max_ms = w.depth_max_ms.max(depth);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::fake::FakeAudio;
    use crate::platform::AudioOut;

    /// Fed audio comes out of the port in whole grains, primed before the first non-silent one.
    #[test]
    fn fed_audio_reaches_the_port_in_grains() {
        let audio = FakeAudio::default();
        let sink = AudioOutSink::start(audio.open(2, 48_000).unwrap(), 2, SyncCells::default()).unwrap();
        let tone = vec![0.5f32; 240 * 2];
        for _ in 0..40 {
            sink.feed(Samples::F32(&tone), 0).unwrap();
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if audio.grains.lock().unwrap().iter().any(|g| g.contains(&0.5)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(sink);
        let grains = audio.grains.lock().unwrap();
        assert!(grains.iter().all(|g| g.len() == 512));
        assert!(
            grains.iter().any(|g| g.contains(&0.5)),
            "no fed sample reached the port"
        );
    }
}
