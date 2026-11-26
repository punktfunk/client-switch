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
//! [`Pacing::Late`] also sleeps into the refresh before the picture is taken: up to a limit the
//! stream finds (`Limit`), never past the GPU's slowest recent frame plus `MARGIN` before the
//! vblank, and under the latency intent clear of the pictures' arrivals (`steer`).
//!
//! Only a stream paces on the vblank. The menu paces on the acquire: its frames carry no picture
//! whose lateness counts, and the two frames queued hide the slow ones a tab change draws, which
//! with one queued each repeat a refresh.
//!
//! `present[10s]` logs the waits, acquires and presents, the frames found queued, where the
//! picture was taken, the limit, the arrivals, and the quick acquires after a stall.

use std::time::{Duration, Instant};

use crate::device::Vsync;
use crate::steer::Steer;

/// How the loop meets the vblank. Without a vsync event every mode paces on the acquire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pacing {
    /// The acquire blocks until the panel frees an image.
    Acquire,
    /// One present after each vblank, the picture taken at once.
    Vblank,
    /// One present after each vblank, the picture taken as late as the stream proves safe.
    Late,
    /// As [`Pacing::Late`], the take at most `START` into the refresh and never steered: the A/B
    /// baseline for the limit the stream finds.
    Fixed,
}

/// Room past the GPU's slowest recent frame for recording, the submit, the present's IPC and a
/// GPU that starts behind a decode lane's copy. A picture past the vblank waits a whole refresh.
/// A present further than this past its take stalled, and tells nothing about the limit.
const MARGIN: Duration = Duration::from_millis(3);
/// Where the limit starts, and where [`Pacing::Fixed`] keeps it.
const START: Duration = Duration::from_millis(10);
/// How far the limit moves at a time: wider than the take's own jitter of about 0.1 ms, so a
/// miss names one offset.
const STEP: Duration = Duration::from_micros(250);
/// Clean presents at the limit before it moves later, about ten seconds at 60 Hz: an offset that
/// misses one present in 200 shows a miss within a run 19 times in 20.
const RUN: u32 = 600;
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
    /// A stream shows: `pacing` applies. Without one every frame paces on the acquire.
    streaming: bool,
    /// `None` paces on the acquire: no event, or one that went quiet.
    vsync: Option<Vsync>,
    queued: Option<fn() -> bool>,
    announced: bool,
    /// Why the platform has no vsync event, for the announcement.
    refused: Option<String>,
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
    /// The latest the late take may sit, found since the GPU readings last reset.
    limit: Limit,
    /// The GPU's room at the last take, for the report.
    last_room: Option<Duration>,
    /// This frame's take, when it sat at the limit.
    this: Option<AtLimit>,
    /// The last frame's, judged at this frame's present.
    prev: Option<AtLimit>,
    /// Where the latency intent takes its picture against the arrivals.
    steer: Steer,
    /// The slot runs the latency intent: the take keeps clear of the arrivals.
    steering: bool,
    window: Window,
}

/// The latest the take may sit after the vblank, found in the stream. It starts at [`START`]. A
/// run of [`RUN`] clean presents taken at it moves it [`STEP`] later, never past the GPU's room. A
/// miss steps it back below the offset that missed, and it never reaches that offset again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Limit {
    at: Duration,
    /// The earliest offset that missed.
    missed: Option<Duration>,
    /// Clean presents at `at` since the run last ended.
    clean: u32,
}

impl Limit {
    const fn new() -> Self {
        Self {
            at: START,
            missed: None,
            clean: 0,
        }
    }

    /// A present taken at the limit latched in time, with `room` the GPU's latest. `true` when
    /// that ends a run and the limit moves later.
    fn clean(&mut self, room: Duration) -> bool {
        self.clean += 1;
        if self.clean < RUN {
            return false;
        }
        self.clean = 0;
        let next = self.at + STEP;
        if next > room || self.missed.is_some_and(|m| next >= m) {
            return false;
        }
        self.at = next;
        true
    }

    /// A present taken at `at` missed its vblank.
    fn miss(&mut self, at: Duration) {
        self.missed = Some(self.missed.map_or(at, |m| m.min(at)));
        self.at = self.at.min(at.saturating_sub(STEP));
        self.clean = 0;
    }
}

/// A take at the limit, judged by the next present.
#[derive(Clone, Copy)]
struct AtLimit {
    /// The limit, after the vblank.
    at: Duration,
    /// The vblank plus `at`.
    target: Instant,
    /// The GPU's latest at the take.
    room: Duration,
    /// Its present came more than [`MARGIN`] after `target`.
    stalled: bool,
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
    /// The limit's steps later, its misses, and the frames queued after a stall.
    later: u32,
    missed: u32,
    stalled: u32,
}

impl Pace {
    /// Pacing on `vsync` where the target has one, and reading `queued` after each present.
    pub fn new(vsync: Option<Vsync>, queued: Option<fn() -> bool>) -> Self {
        let samples = || Vec::with_capacity(SAMPLES);
        Self {
            pacing: Pacing::Late,
            streaming: false,
            vsync,
            queued,
            announced: false,
            refused: None,
            gpu: [0; GPU_FRAMES],
            gpu_n: 0,
            drain: false,
            drained: false,
            drains: true,
            quiet: 0,
            burst: None,
            limit: Limit::new(),
            last_room: None,
            this: None,
            prev: None,
            steer: Steer::new(),
            steering: false,
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

    /// Paces on `pacing` while a stream shows, on the acquire otherwise. A stream starts with no
    /// stale vblank and no drain left from the menu's frames.
    pub fn set_streaming(&mut self, on: bool) {
        if on && !self.streaming {
            self.drain = false;
            self.drained = false;
            self.burst = None;
            if let Some(v) = self.vsync.filter(|_| self.pacing != Pacing::Acquire) {
                (v.clear)();
            }
        }
        self.streaming = on;
    }

    /// Why the platform could hand over no vsync event.
    pub fn set_refused(&mut self, why: String) {
        self.refused = Some(why);
    }

    fn vsync(&self) -> Option<Vsync> {
        self.vsync.filter(|_| self.streaming && self.pacing != Pacing::Acquire)
    }

    /// Logs the mode the loop runs in, once.
    pub fn announce(&mut self) {
        if std::mem::replace(&mut self.announced, true) {
            return;
        }
        let how = match (self.pacing, self.vsync.is_some()) {
            (Pacing::Acquire, _) => "paced by the acquire, as asked",
            (_, false) => "paced by the acquire: the display gave no vsync event",
            (Pacing::Vblank, true) => "a stream paced on the vblank, one frame queued at most; the menu by the acquire",
            (Pacing::Late, true) => {
                "a stream paced on the vblank, one frame queued at most, the picture taken late; the menu by the acquire"
            }
            (Pacing::Fixed, true) => {
                "a stream paced on the vblank, one frame queued at most, the picture taken late at a fixed limit; \
                 the menu by the acquire"
            }
        };
        match &self.refused {
            Some(why) if self.vsync.is_none() => tracing::warn!("presenter: {how}: {why}"),
            _ => tracing::info!("presenter: {how}"),
        }
    }

    /// One command buffer's GPU time, for the late take's budget.
    pub fn note_gpu(&mut self, us: u32) {
        self.gpu[self.gpu_n % GPU_FRAMES] = us;
        self.gpu_n += 1;
    }

    /// Forgets what the late take learned: the GPU readings, the limit and the arrivals. The
    /// stream, or the size the frames are drawn at, changed.
    pub fn reset_take(&mut self) {
        self.gpu_n = 0;
        self.limit = Limit::new();
        self.this = None;
        self.prev = None;
        self.steer.reset();
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

    /// The pictures that reached the slot since the last take, against the vblank at `vblank`.
    /// `latency`: the slot runs newest wins, and the late take keeps clear of them.
    pub fn arrived(&mut self, vblank: Instant, period: Duration, arrivals: &[Instant], latency: bool) {
        self.steering = latency;
        if latency {
            self.steer.note(vblank, period, arrivals);
        }
    }

    /// Late pacing: sleeps after `vblank` until the limit, or earlier where the GPU's room or,
    /// under the latency intent, the arrivals ask. Notes where in the refresh the picture is taken
    /// either way.
    pub fn take(&mut self, vblank: Instant, period: Duration) {
        let limit = match self.pacing {
            Pacing::Late => Some(self.limit.at),
            Pacing::Fixed => Some(START),
            Pacing::Acquire | Pacing::Vblank => None,
        };
        self.last_room = self.room(period);
        if let (Some(limit), Some(room)) = (limit, self.last_room) {
            let latest = limit.min(room);
            let at = if self.steering && self.pacing == Pacing::Late {
                self.steer.offset(latest)
            } else {
                latest
            };
            let target = vblank + at;
            if let Some(rest) = target.checked_duration_since(Instant::now()) {
                std::thread::sleep(rest);
            }
            self.this = (at == limit).then_some(AtLimit {
                at,
                target,
                room,
                stalled: false,
            });
        }
        push(&mut self.window.taken, vblank.elapsed());
    }

    /// The latest the take can sit for the GPU: the period less the slowest recent GPU frame and
    /// [`MARGIN`]. `None` until the readings fill, or with no room left.
    fn room(&self, period: Duration) -> Option<Duration> {
        if self.gpu_n < GPU_FRAMES {
            return None;
        }
        let worst = Duration::from_micros(u64::from(self.gpu.iter().copied().max()?));
        period.checked_sub(worst + MARGIN)
    }

    /// Call right after the present returns, `took` after it began: reads whether it found a
    /// frame queued, judges the last frame's take by it, and clears the vsync event so the next
    /// wait is for a vblank after it.
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
        self.judge(behind);
        self.window.frames += 1;
        self.window.behind += u32::from(behind);
        push(&mut self.window.present, took);
    }

    /// A frame taken at the limit and still queued at the next present, also taken at the limit,
    /// missed its vblank. Nothing is judged across a drained wait or once drains stop, and a frame
    /// whose present stalled is no miss. [`Pacing::Fixed`] counts misses and keeps its limit.
    fn judge(&mut self, behind: bool) {
        let now = Instant::now();
        let this = self.this.take().map(|t| AtLimit {
            stalled: now > t.target + MARGIN,
            ..t
        });
        let prev = std::mem::replace(&mut self.prev, this);
        let (Some(prev), Some(this)) = (prev, this) else {
            return;
        };
        if this.stalled || self.drained || !self.drains {
            return;
        }
        let late = self.pacing == Pacing::Late;
        match (behind, prev.stalled) {
            (false, false) => {
                if late && self.limit.clean(this.room) {
                    self.window.later += 1;
                }
            }
            (false, true) => {}
            (true, true) => self.window.stalled += 1,
            (true, false) => {
                self.window.missed += 1;
                if late {
                    self.limit.miss(prev.at);
                }
            }
        }
    }

    /// The window's line once it has run [`WINDOW`], and a fresh window.
    pub fn close(&mut self, now: Instant) -> Option<String> {
        let since = *self.window.since.get_or_insert(now);
        if now.duration_since(since) < WINDOW {
            return None;
        }
        let vblank = self.vsync().is_some();
        let mode = match (vblank, self.pacing) {
            (false, _) => "on the acquire",
            (true, Pacing::Late | Pacing::Fixed) => "on the vblank, taken late",
            (true, _) => "on the vblank",
        };
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        let w = &self.window;
        let room = self
            .last_room
            .map_or_else(|| "-".into(), |r| format!("{:.2} ms", ms(r)));
        let judged = format!(
            "{} missed, {} queued after a stall, GPU room {room}",
            w.missed, w.stalled
        );
        let limit = match (vblank, self.pacing) {
            (true, Pacing::Late) => format!("; limit {:.2} ms, {} later, {judged}", ms(self.limit.at), w.later),
            (true, Pacing::Fixed) => format!("; limit fixed at {:.2} ms, {judged}", ms(START)),
            _ => String::new(),
        };
        let arrivals = self
            .steer
            .summary(self.steering && self.pacing == Pacing::Late)
            .filter(|_| vblank)
            .map_or_else(String::new, |s| format!("; {s}"));
        let w = &mut self.window;
        let b = w.bursts;
        let line = format!(
            "present[{}s]: {} frames {mode}; vblank wait {}, {} timed out; acquire {}, {} under 1 ms; present {}; \
             {} found a frame queued, {} drained; picture taken {} after the vblank; quick acquires after a stall \
             0:{} 1:{} 2:{} 3:{} {}+:{}{limit}{arrivals}",
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

    static MENU_WAITS: AtomicU32 = AtomicU32::new(0);
    fn menu_wait(_: u64) -> bool {
        MENU_WAITS.fetch_add(1, Ordering::Relaxed);
        true
    }
    fn menu_behind() -> bool {
        true
    }

    /// The GPU's room leaves the slowest of the last readings plus the margin, and is none until
    /// the readings fill or with no room left.
    #[test]
    fn room_leaves_the_slowest_frame_and_the_margin() {
        let mut pace = Pace::new(None, None);
        assert_eq!(pace.room(PERIOD), None);
        for _ in 0..GPU_FRAMES {
            pace.note_gpu(6_000);
        }
        pace.note_gpu(6_700);
        assert_eq!(pace.room(PERIOD), Some(PERIOD - Duration::from_micros(6_700) - MARGIN));
        pace.reset_take();
        assert_eq!(pace.room(PERIOD), None);
        for _ in 0..GPU_FRAMES {
            pace.note_gpu(15_000);
        }
        assert_eq!(pace.room(PERIOD), None);
    }

    const ROOM: Duration = Duration::from_millis(13);

    /// A run of clean presents moves the limit one step later; one short of a run does not.
    #[test]
    fn the_limit_walks_later_on_clean_presents() {
        let mut limit = Limit::new();
        for _ in 1..RUN {
            assert!(!limit.clean(ROOM));
        }
        assert!(limit.clean(ROOM));
        assert_eq!(limit.at, START + STEP);
    }

    /// A miss steps the limit back below the offset that missed, and no run reaches it again,
    /// at the start as anywhere.
    #[test]
    fn a_miss_steps_back_for_good() {
        let mut limit = Limit::new();
        for _ in 0..RUN * 2 {
            limit.clean(ROOM);
        }
        assert_eq!(limit.at, START + STEP * 2);
        limit.miss(limit.at);
        assert_eq!(limit.at, START + STEP);
        for _ in 0..RUN * 20 {
            limit.clean(ROOM);
        }
        assert_eq!(limit.at, START + STEP);
        limit.miss(START);
        assert_eq!(limit.at, START - STEP);
        for _ in 0..RUN * 20 {
            limit.clean(ROOM);
        }
        assert_eq!(limit.at, START - STEP);
    }

    /// The limit never walks past the GPU's room.
    #[test]
    fn the_limit_never_passes_the_gpu_room() {
        let mut limit = Limit::new();
        let room = START + STEP + STEP / 2;
        for _ in 0..RUN * 20 {
            limit.clean(room);
        }
        assert_eq!(limit.at, START + STEP);
    }

    thread_local! {
        static QUEUED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    fn queued() -> bool {
        QUEUED.with(std::cell::Cell::get)
    }
    fn at_once(_: u64) -> bool {
        true
    }

    /// A stream paced on the vblank, `pacing`, with GPU readings that leave 12.7 ms of room.
    fn streaming(pacing: Pacing) -> Pace {
        let mut pace = Pace::new(Some(Vsync { clear, wait: at_once }), Some(queued));
        pace.set(pacing);
        pace.set_streaming(true);
        for _ in 0..GPU_FRAMES {
            pace.note_gpu(1_000);
        }
        pace
    }

    /// One frame taken at its limit, its present `late` past the take, finding a frame queued
    /// when `found`. Its present judges the frame before it.
    fn frame(pace: &mut Pace, late: Duration, found: bool) {
        pace.wait(PERIOD);
        let at = if pace.pacing == Pacing::Fixed {
            START
        } else {
            pace.limit.at
        };
        pace.take(Instant::now() - at - late, PERIOD);
        QUEUED.with(|q| q.set(found));
        pace.presented(Duration::ZERO);
    }

    /// Clean presents walk the limit later; a frame taken there and still queued at the next
    /// present steps it back; a new stream starts it over.
    #[test]
    fn a_frame_queued_after_a_late_take_is_a_miss() {
        let mut pace = streaming(Pacing::Late);
        for _ in 0..=RUN {
            frame(&mut pace, Duration::ZERO, false);
        }
        assert_eq!((pace.limit.at, pace.window.later), (START + STEP, 1));
        frame(&mut pace, Duration::ZERO, false);
        frame(&mut pace, Duration::ZERO, true);
        assert_eq!((pace.limit.at, pace.window.missed), (START, 1));
        assert_eq!(pace.limit.missed, Some(START + STEP));
        pace.reset_take();
        assert_eq!(pace.limit, Limit::new());
    }

    /// The frame after a miss waits two vblanks: queued or not, it judges nothing, and once the
    /// drains stop nothing is judged at all.
    #[test]
    fn a_drained_wait_judges_nothing() {
        let mut pace = streaming(Pacing::Late);
        frame(&mut pace, Duration::ZERO, false);
        frame(&mut pace, Duration::ZERO, true);
        assert_eq!((pace.limit.at, pace.window.missed), (START - STEP, 1));
        frame(&mut pace, Duration::ZERO, true);
        assert!(!pace.drains);
        frame(&mut pace, Duration::ZERO, true);
        frame(&mut pace, Duration::ZERO, false);
        assert_eq!(
            (pace.limit.at, pace.limit.clean, pace.window.missed),
            (START - STEP, 0, 1)
        );
    }

    /// A frame whose present came more than the margin after its take is no miss, and a present
    /// that stalled judges nothing.
    #[test]
    fn a_stalled_present_is_no_miss() {
        let mut pace = streaming(Pacing::Late);
        let stall = MARGIN + Duration::from_millis(1);
        frame(&mut pace, stall, false);
        frame(&mut pace, Duration::ZERO, true);
        assert_eq!((pace.window.missed, pace.window.stalled), (0, 1));
        frame(&mut pace, Duration::ZERO, false);
        frame(&mut pace, stall, true);
        assert_eq!((pace.limit, pace.window.missed), (Limit::new(), 0));
    }

    /// The fixed take counts its misses and keeps its limit.
    #[test]
    fn fixed_pacing_counts_misses_but_keeps_its_limit() {
        let mut pace = streaming(Pacing::Fixed);
        frame(&mut pace, Duration::ZERO, false);
        frame(&mut pace, Duration::ZERO, true);
        assert_eq!((pace.limit, pace.window.missed), (Limit::new(), 1));
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

    /// The menu paces on the acquire: no vblank waits, no drain. A stream paces on the vblank,
    /// and after it the menu is back on the acquire.
    #[test]
    fn only_a_stream_paces_on_the_vblank() {
        let vsync = Vsync { clear, wait: menu_wait };
        let mut pace = Pace::new(Some(vsync), Some(menu_behind));
        assert_eq!(pace.wait(PERIOD), None);
        pace.presented(Duration::ZERO);
        assert!(!pace.drain);
        assert_eq!(MENU_WAITS.load(Ordering::Relaxed), 0);
        pace.set_streaming(true);
        pace.wait(PERIOD);
        assert_eq!(MENU_WAITS.load(Ordering::Relaxed), 1);
        pace.set_streaming(false);
        assert_eq!(pace.wait(PERIOD), None);
        assert_eq!(MENU_WAITS.load(Ordering::Relaxed), 1);
    }

    /// A present that finds a frame queued makes the next frame wait two vblanks; one that still
    /// does after that stops the draining.
    #[test]
    fn a_queued_frame_drains_once_then_stops_if_it_stays() {
        let vsync = Vsync { clear, wait };
        let mut pace = Pace::new(Some(vsync), Some(behind));
        pace.set_streaming(true);
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
