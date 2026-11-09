//! How the presenter meets the vblank, and the figures that show how deep the queue runs.
//!
//! NVK's vi swapchain has three images, and the acquire returns once the panel frees any. Paced
//! by the acquire, the loop keeps every free image queued, so a picture can wait behind another
//! frame for a second refresh. Paced on the vblank, the vsync event is cleared after each present
//! and the next acquire follows the next vblank: the frame queued before it has latched, so at
//! most one waits. A present that still finds one queued (libnx's consumer-running-behind) makes
//! the next frame wait two vblanks. A wait that times out paces that frame on the acquire; a run
//! of them, every frame after.
//!
//! [`Pacing::Late`] also sleeps into the refresh before the picture is taken, leaving the GPU's
//! slowest recent frame plus `MARGIN` before the vblank.
//!
//! `present[10s]` logs the waits, the acquires and the presents, how many presents found a frame
//! queued, where in the refresh the picture was taken, and, paced by the acquire, how many
//! acquires returned at once after a stall long enough to empty the queue: as many as it takes.

use std::time::{Duration, Instant};

use crate::device::Vsync;

/// How the loop meets the vblank. Without a vsync event every mode paces on the acquire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pacing {
    /// The acquire blocks until the panel frees an image.
    Acquire,
    /// One present after each vblank, the picture taken at once.
    Vblank,
    /// One present after each vblank, the picture taken as late as the composition allows.
    Late,
}

/// Room past the GPU's slowest recent frame for recording, the submit, the present's IPC and a
/// GPU that starts behind a decode lane's copy. A picture past the vblank waits a whole refresh.
const MARGIN: Duration = Duration::from_millis(3);
/// The furthest the take moves into a refresh: GPU readings trail by two frames, and a dearer
/// picture (a dock change, the overlay turned on) must still fit before a reading shows it.
const MAX_LATE: Duration = Duration::from_millis(10);
/// GPU readings the budget looks back over, about half a second; it sleeps only once it has all.
const GPU_FRAMES: usize = 32;
/// A wait or an acquire shorter than this did not block.
const QUICK: Duration = Duration::from_millis(1);
/// A vsync wait gives up after this many refreshes, so sleep or a dead event never hangs a frame.
const WAIT_PERIODS: u32 = 3;
/// Timed-out waits in a row after which every frame paces on the acquire.
const QUIET_WAITS: u32 = 8;
const WINDOW: Duration = Duration::from_secs(10);
/// Samples a window keeps of each figure; ten seconds at 60 Hz is 600.
const SAMPLES: usize = 1024;
/// Runs of quick acquires after a stall are counted up to this, the last bucket holding the rest.
const BURST_MAX: usize = 4;

pub(crate) struct Pace {
    pacing: Pacing,
    /// `None` paces on the acquire: no event, or one that went quiet.
    vsync: Option<Vsync>,
    queued: Option<fn() -> bool>,
    announced: bool,
    /// The last [`GPU_FRAMES`] command buffers' GPU time, µs.
    gpu: [u32; GPU_FRAMES],
    gpu_n: usize,
    /// The last present found another frame queued: the next frame waits two vblanks.
    drain: bool,
    /// This frame waited two; a present that still finds one queued stops the draining.
    drained: bool,
    drains: bool,
    quiet: u32,
    /// Quick acquires since a stall emptied the queue, while the run lasts.
    burst: Option<usize>,
    window: Window,
}

#[derive(Default)]
struct Window {
    since: Option<Instant>,
    frames: u32,
    /// µs each.
    wait: Vec<u32>,
    acquire: Vec<u32>,
    present: Vec<u32>,
    /// From the vblank to the picture's take.
    taken: Vec<u32>,
    quick: u32,
    behind: u32,
    drained: u32,
    timeouts: u32,
    bursts: [u32; BURST_MAX + 1],
}

impl Pace {
    /// Pacing on `vsync` where the target has one, and reading `queued` after each present.
    pub fn new(vsync: Option<Vsync>, queued: Option<fn() -> bool>) -> Self {
        let samples = || Vec::with_capacity(SAMPLES);
        Self {
            pacing: Pacing::Late,
            vsync,
            queued,
            announced: false,
            gpu: [0; GPU_FRAMES],
            gpu_n: 0,
            drain: false,
            drained: false,
            drains: true,
            quiet: 0,
            burst: None,
            window: Window {
                wait: samples(),
                acquire: samples(),
                present: samples(),
                taken: samples(),
                ..Window::default()
            },
        }
    }

    pub fn set(&mut self, pacing: Pacing) {
        self.pacing = pacing;
    }

    fn vsync(&self) -> Option<Vsync> {
        self.vsync.filter(|_| self.pacing != Pacing::Acquire)
    }

    /// Logs the mode the loop runs in, once.
    pub fn announce(&mut self) {
        if std::mem::replace(&mut self.announced, true) {
            return;
        }
        let how = match (self.pacing, self.vsync.is_some()) {
            (Pacing::Acquire, _) => "paced by the acquire, as asked",
            (_, false) => "paced by the acquire: the display gave no vsync event",
            (Pacing::Vblank, true) => "paced on the vblank, one frame queued at most",
            (Pacing::Late, true) => "paced on the vblank, one frame queued at most, the picture taken late",
        };
        tracing::info!("presenter: {how}");
    }

    /// One command buffer's GPU time, for the late take's budget.
    pub fn note_gpu(&mut self, us: u32) {
        self.gpu[self.gpu_n % GPU_FRAMES] = us;
        self.gpu_n += 1;
    }

    /// Forgets the GPU readings: what the frames draw, or at what size, changed.
    pub fn reset_gpu(&mut self) {
        self.gpu_n = 0;
    }

    /// Waits for the next vblank when the loop paces on it: the instant it came, where the wait
    /// blocked long enough to say. `None` paces this frame on the acquire, or the vblank fell
    /// before the wait and its time is unknown.
    pub fn wait(&mut self, period: Duration) -> Option<Instant> {
        let v = self.vsync()?;
        let timeout = u64::try_from((period * WAIT_PERIODS).as_nanos()).unwrap_or(u64::MAX);
        let start = Instant::now();
        let mut came = (v.wait)(timeout);
        self.drained = came && std::mem::take(&mut self.drain);
        if self.drained {
            self.window.drained += 1;
            came = (v.wait)(timeout);
        }
        let woke = Instant::now();
        push(&mut self.window.wait, woke - start);
        if !came {
            self.window.timeouts += 1;
            self.quiet += 1;
            if self.quiet == QUIET_WAITS {
                self.vsync = None;
                tracing::warn!("presenter: no vblank in {QUIET_WAITS} waits running, paced by the acquire from here");
            }
            return None;
        }
        self.quiet = 0;
        (woke - start >= QUICK).then_some(woke)
    }

    /// Notes an acquire that blocked for `blocked`, `since_present` after the last present. Paced
    /// by the acquire, a gap over two refreshes starts a run of quick acquires. `false` when the
    /// acquire blocked.
    pub fn acquired(&mut self, blocked: Duration, since_present: Option<Duration>, period: Duration) -> bool {
        push(&mut self.window.acquire, blocked);
        let quick = blocked < QUICK;
        self.window.quick += u32::from(quick);
        if self.vsync().is_some() {
            self.burst = None;
            return quick;
        }
        if since_present.is_none_or(|gap| gap > period * 2) {
            self.burst = Some(0);
        }
        if let Some(n) = self.burst {
            let n = n + usize::from(quick);
            if quick && n < BURST_MAX {
                self.burst = Some(n);
            } else {
                self.window.bursts[n] += 1;
                self.burst = None;
            }
        }
        quick
    }

    /// Late pacing: sleeps until the composition's budget before the vblank after `vblank`. Notes
    /// where in the refresh the picture is taken either way.
    pub fn take(&mut self, vblank: Instant, period: Duration) {
        if self.pacing == Pacing::Late {
            if let Some(rest) = self
                .late_offset(period)
                .and_then(|at| (vblank + at).checked_duration_since(Instant::now()))
            {
                std::thread::sleep(rest);
            }
        }
        push(&mut self.window.taken, vblank.elapsed());
    }

    /// How far into a refresh the take can wait: the period less the slowest recent GPU frame and
    /// [`MARGIN`], at most [`MAX_LATE`]. `None` until the readings fill, or with no room left.
    fn late_offset(&self, period: Duration) -> Option<Duration> {
        if self.gpu_n < GPU_FRAMES {
            return None;
        }
        let worst = Duration::from_micros(u64::from(self.gpu.iter().copied().max()?));
        period.checked_sub(worst + MARGIN).map(|at| at.min(MAX_LATE))
    }

    /// Call right after the present returns, `took` after it began: reads whether it found a
    /// frame queued, and clears the vsync event so the next wait is for a vblank after it.
    pub fn presented(&mut self, took: Duration) {
        let behind = self.queued.is_some_and(|queued| queued());
        if let Some(v) = self.vsync() {
            (v.clear)();
            if behind && self.drains {
                if self.drained {
                    self.drains = false;
                    tracing::warn!("presenter: a frame still queued after a drained vblank, so no more drains");
                } else {
                    self.drain = true;
                }
            }
        }
        self.window.frames += 1;
        self.window.behind += u32::from(behind);
        push(&mut self.window.present, took);
    }

    /// The window's line once it has run [`WINDOW`], and a fresh window.
    pub fn close(&mut self, now: Instant) -> Option<String> {
        let since = *self.window.since.get_or_insert(now);
        if now.duration_since(since) < WINDOW {
            return None;
        }
        let mode = match (self.vsync(), self.pacing) {
            (None, _) => "on the acquire",
            (Some(_), Pacing::Late) => "on the vblank, taken late",
            (Some(_), _) => "on the vblank",
        };
        let w = &mut self.window;
        let b = w.bursts;
        let line = format!(
            "present[{}s]: {} frames {mode}; vblank wait {}, {} timed out; acquire {}, {} under 1 ms; present {}; \
             {} found a frame queued, {} drained; picture taken {} after the vblank; quick acquires after a stall \
             0:{} 1:{} 2:{} 3:{} {}+:{}",
            WINDOW.as_secs(),
            w.frames,
            spread(&mut w.wait),
            w.timeouts,
            spread(&mut w.acquire),
            w.quick,
            spread(&mut w.present),
            w.behind,
            w.drained,
            spread(&mut w.taken),
            b[0],
            b[1],
            b[2],
            b[3],
            BURST_MAX,
            b[4],
        );
        let keep = |v: &mut Vec<u32>| {
            v.clear();
            std::mem::take(v)
        };
        *w = Window {
            since: Some(now),
            wait: keep(&mut w.wait),
            acquire: keep(&mut w.acquire),
            present: keep(&mut w.present),
            taken: keep(&mut w.taken),
            ..Window::default()
        };
        Some(line)
    }
}

/// Keeps `d` in µs while the window has room: the capacity is reserved once, so no frame allocates.
fn push(v: &mut Vec<u32>, d: Duration) {
    if v.len() < SAMPLES {
        v.push(u32::try_from(d.as_micros()).unwrap_or(u32::MAX));
    }
}

/// `p50 … p95 … max … ms`, or `-` for none; sorts `v`.
fn spread(v: &mut [u32]) -> String {
    if v.is_empty() {
        return "-".into();
    }
    v.sort_unstable();
    let ms = |us: u32| f64::from(us) / 1000.0;
    format!(
        "p50 {:.1} p95 {:.1} max {:.1} ms",
        ms(v[v.len() / 2]),
        ms(v[v.len() * 95 / 100]),
        ms(v[v.len() - 1])
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    const PERIOD: Duration = Duration::from_nanos(16_666_667);

    static WAITS: AtomicU32 = AtomicU32::new(0);
    static BEHIND: AtomicU32 = AtomicU32::new(0);

    fn clear() {}
    fn wait(_: u64) -> bool {
        WAITS.fetch_add(1, Ordering::Relaxed);
        true
    }
    fn behind() -> bool {
        BEHIND.load(Ordering::Relaxed) != 0
    }

    /// The take waits for the slowest of the last readings plus the margin, never past
    /// [`MAX_LATE`], and not at all until the readings fill or with no room left.
    #[test]
    fn late_offset_leaves_the_slowest_frame_and_the_margin() {
        let mut pace = Pace::new(None, None);
        assert_eq!(pace.late_offset(PERIOD), None);
        for _ in 0..GPU_FRAMES {
            pace.note_gpu(6_000);
        }
        pace.note_gpu(6_700);
        let at = pace.late_offset(PERIOD).expect("room");
        assert_eq!(at, PERIOD - Duration::from_micros(6_700) - MARGIN);
        pace.reset_gpu();
        for _ in 0..GPU_FRAMES {
            pace.note_gpu(1_000);
        }
        assert_eq!(pace.late_offset(PERIOD), Some(MAX_LATE));
        pace.note_gpu(15_000);
        assert_eq!(pace.late_offset(PERIOD), None);
    }

    /// Paced by the acquire, a stall starts a run that ends at the first blocked acquire.
    #[test]
    fn a_stall_counts_the_quick_acquires_after_it() {
        let mut pace = Pace::new(None, None);
        let (quick, blocked) = (Duration::ZERO, Duration::from_millis(12));
        pace.acquired(quick, Some(PERIOD * 3), PERIOD);
        pace.acquired(quick, Some(Duration::from_millis(4)), PERIOD);
        pace.acquired(blocked, Some(Duration::from_millis(4)), PERIOD);
        pace.acquired(blocked, Some(Duration::from_millis(4)), PERIOD);
        assert_eq!(pace.window.bursts, [0, 0, 1, 0, 0]);
        pace.acquired(quick, None, PERIOD);
        for _ in 0..6 {
            pace.acquired(quick, Some(Duration::from_millis(4)), PERIOD);
        }
        assert_eq!(pace.window.bursts, [0, 0, 1, 0, 1]);
    }

    /// A present that finds a frame queued makes the next frame wait two vblanks; one that still
    /// does after that stops the draining.
    #[test]
    fn a_queued_frame_drains_once_then_stops_if_it_stays() {
        let vsync = Vsync { clear, wait };
        let mut pace = Pace::new(Some(vsync), Some(behind));
        BEHIND.store(1, Ordering::Relaxed);
        WAITS.store(0, Ordering::Relaxed);
        pace.wait(PERIOD);
        pace.presented(Duration::ZERO);
        assert!(pace.drain);
        pace.wait(PERIOD);
        assert_eq!(WAITS.load(Ordering::Relaxed), 3, "the drained frame waits twice");
        pace.presented(Duration::ZERO);
        assert!(!pace.drains && !pace.drain);
        BEHIND.store(0, Ordering::Relaxed);
    }
}
