//! Where in the refresh the latency intent takes its picture.
//!
//! Newest wins: a picture that lands just after the take waits a whole refresh, and the one after
//! it lands before the next take and replaces it unseen. The host's frame clock and the panel's
//! drift apart, so the arrivals walk through the refresh, about 0.8 ms a minute in issue 30's run.
//! A take at a fixed point is crossed every twenty minutes or so, and while the band of arrivals
//! straddles it, up to a third of the pictures go unseen for minutes.
//!
//! The steer follows the arrivals instead. It places the take [`GUARD`] past the latest usual
//! arrival, within what the GPU budget allows. It moves at once when the arrivals come within
//! [`UNSAFE`] of the take, and moves earlier when that would cut the wait by more than [`SLACK`].
//! A band that runs past the budget is taken at the start of the next refresh.

use std::time::{Duration, Instant};

/// Arrivals the band is read over: about two seconds at 60 pictures a second.
const ARRIVALS: usize = 128;
/// Arrivals between looks at the take: about a second.
const EVERY: usize = 60;
/// Room the take leaves past the latest usual arrival.
const GUARD: Duration = Duration::from_millis(2);
/// A take this close to the latest usual arrival moves at once.
const UNSAFE: Duration = Duration::from_millis(1);
/// A take that could wait this much less moves earlier.
const SLACK: Duration = Duration::from_millis(3);
/// Arrivals the band holds, in percent of the window; the rest are a late burst it lets go.
const BAND_PCT: usize = 98;

pub(crate) struct Steer {
    /// Each arrival's place in the refresh, µs after the vblank, the last [`ARRIVALS`].
    phases: [u32; ARRIVALS],
    n: usize,
    /// Arrivals since the last look.
    since: usize,
    /// The take's place in the refresh; `None` until the first look, which starts from the budget.
    at: Option<Duration>,
    /// The band at the last look: its centre and its width.
    band: Option<(Duration, Duration)>,
    /// Moves since the last report.
    moves: u32,
}

/// A band of arrivals, in µs: where its earliest usual arrival sits and how wide it runs.
struct Band {
    earliest: u64,
    width: u64,
    centre: u64,
}

impl Steer {
    pub fn new() -> Self {
        Self {
            phases: [0; ARRIVALS],
            n: 0,
            since: 0,
            at: None,
            band: None,
            moves: 0,
        }
    }

    /// Forgets the arrivals: a new stream, a new host clock.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Notes `arrivals` against the vblank at `vblank`, `period` apart.
    pub fn note(&mut self, vblank: Instant, period: Duration, arrivals: &[Instant]) {
        let p = period.as_micros() as i64;
        if p == 0 {
            return;
        }
        for &a in arrivals {
            let off = if a >= vblank {
                (a - vblank).as_micros() as i64
            } else {
                -((vblank - a).as_micros() as i64)
            };
            self.phases[self.n % ARRIVALS] = off.rem_euclid(p) as u32;
            self.n += 1;
            self.since += 1;
        }
    }

    /// The take's place in the refresh, at most `max`, the GPU budget's latest.
    pub fn offset(&mut self, max: Duration, period: Duration) -> Duration {
        if self.n >= EVERY && self.since >= EVERY {
            self.since = 0;
            self.look(max, period);
        }
        self.at.unwrap_or(max).min(max)
    }

    fn look(&mut self, max: Duration, period: Duration) {
        let p = period.as_micros() as u64;
        let held = self.n.min(ARRIVALS);
        let Some(band) = band(&self.phases[..held], p) else {
            return;
        };
        self.band = Some((Duration::from_micros(band.centre), Duration::from_micros(band.width)));
        let (guard, unsafe_, slack) = (us(GUARD), us(UNSAFE), us(SLACK));
        if band.width + 2 * guard >= p {
            // No point in the refresh clears the band: leave the take where it is.
            return;
        }
        let max = us(max).min(p - 1);
        // How far past the band's latest usual arrival a take at `t` sits; 0 inside the band.
        let margin = |t: u64| ((t + p - band.earliest) % p).saturating_sub(band.width);
        let want = (band.earliest + band.width + guard) % p;
        let target = if want <= max { want } else { 0 };
        let cur = self.at.map_or(max, |d| us(d).min(max));
        let cur_margin = margin(cur);
        let move_now = cur_margin < unsafe_ || (self.at.is_none() && cur != target);
        let saves = cur_margin.saturating_sub(margin(target)) > slack;
        if (move_now || saves) && cur != target {
            self.moves += u32::from(self.at.is_some());
            self.at = Some(Duration::from_micros(target));
        } else if self.at.is_none() {
            self.at = Some(Duration::from_micros(cur));
        }
    }

    /// `arrivals at … ms, band … ms, take … ms, … moves`, then a fresh count; `None` before the
    /// first look.
    pub fn summary(&mut self) -> Option<String> {
        let (centre, width) = self.band?;
        let at = self.at?;
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        let line = format!(
            "arrivals at {:.1} ms in a {:.1} ms band, take at {:.1} ms, {} moves",
            ms(centre),
            ms(width),
            ms(at),
            self.moves
        );
        self.moves = 0;
        Some(line)
    }
}

fn us(d: Duration) -> u64 {
    d.as_micros() as u64
}

/// The band the arrivals' `phases` (µs into a `p` µs refresh) make: its centre by the circular
/// mean, then the spread around it that holds [`BAND_PCT`] of them.
fn band(phases: &[u32], p: u64) -> Option<Band> {
    if phases.is_empty() || p == 0 {
        return None;
    }
    let tau = std::f64::consts::TAU;
    let (mut s, mut c) = (0.0, 0.0);
    for &ph in phases {
        let a = f64::from(ph) / p as f64 * tau;
        s += a.sin();
        c += a.cos();
    }
    let centre = (s.atan2(c).rem_euclid(tau) / tau * p as f64) as i64;
    let half = (p / 2) as i64;
    let mut devs: Vec<i64> = phases
        .iter()
        .map(|&ph| (i64::from(ph) - centre + half).rem_euclid(p as i64) - half)
        .collect();
    devs.sort_unstable();
    let cut = |pct: usize| devs[((devs.len() - 1) * pct) / 100];
    let (lo, hi) = (cut(100 - BAND_PCT), cut(BAND_PCT));
    Some(Band {
        earliest: (centre + lo).rem_euclid(p as i64) as u64,
        width: (hi - lo) as u64,
        centre: centre.rem_euclid(p as i64) as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERIOD: Duration = Duration::from_nanos(16_666_667);
    const MAX: Duration = Duration::from_millis(10);

    /// One second of arrivals around `centre_us` into the refresh, spread over ±`jitter_us`.
    fn second(steer: &mut Steer, centre_us: u64, jitter_us: u64) {
        let vblank = Instant::now() + Duration::from_secs(1);
        let arrivals: Vec<Instant> = (0..EVERY as u64)
            .map(|i| {
                let j = if jitter_us == 0 {
                    0
                } else {
                    (i * 7919) % (2 * jitter_us + 1)
                };
                vblank + Duration::from_micros(centre_us + j) - Duration::from_micros(jitter_us)
            })
            .collect();
        steer.note(vblank, PERIOD, &arrivals);
    }

    fn ms(steer: &mut Steer) -> f64 {
        steer.offset(MAX, PERIOD).as_secs_f64() * 1e3
    }

    /// Until the first look the take is the budget's; then it sits the guard past the band.
    #[test]
    fn the_take_starts_at_the_budget_then_follows_the_band() {
        let mut s = Steer::new();
        assert_eq!(s.offset(MAX, PERIOD), MAX);
        second(&mut s, 3_000, 500);
        let at = ms(&mut s);
        assert!((5.3..=5.7).contains(&at), "{at}");
    }

    /// A band that walks through the refresh, earlier as in issue 30's run or later, never comes
    /// within the unsafe distance of the take, and the take steps after it.
    #[test]
    fn a_drifting_band_never_reaches_the_take() {
        for step in [-13, 13] {
            drift(step);
        }
    }

    /// Twenty-five minutes of a band walking `step` µs a second, about a refresh and a half.
    fn drift(step: i64) {
        let mut s = Steer::new();
        let mut centre: i64 = 8_000;
        let mut moves = 0;
        for _ in 0..1_500 {
            let c = centre.rem_euclid(16_667) as u64;
            second(&mut s, c, 800);
            let before = s.at;
            let at = us(s.offset(MAX, PERIOD)) as i64;
            moves += u32::from(before.is_some() && before != s.at);
            // The latest arrival of all, against the take: the band's edge holds 98 of 100, so
            // the take keeps at least half the unsafe distance from the very last.
            let latest = (c as i64 + 800).rem_euclid(16_667);
            let gap = (at - latest).rem_euclid(16_667);
            assert!(gap >= us(UNSAFE) as i64 / 2, "take {at} µs, latest arrival {latest} µs");
            centre += step; // 0.78 ms a minute
        }
        assert!(moves > 0 && moves < 60, "{moves} moves at {step} µs a second");
    }

    /// A band past the budget's latest point is taken at the start of the next refresh.
    #[test]
    fn a_band_past_the_budget_is_taken_at_the_next_refresh() {
        let mut s = Steer::new();
        second(&mut s, 12_000, 500);
        assert_eq!(s.offset(MAX, PERIOD), Duration::ZERO);
    }

    /// A band that comes up on the take moves it at once; a calm one leaves it.
    #[test]
    fn the_take_moves_only_when_unsafe_or_wasteful() {
        let mut s = Steer::new();
        second(&mut s, 3_000, 300);
        let first = s.offset(MAX, PERIOD);
        second(&mut s, 3_500, 300);
        assert_eq!(s.offset(MAX, PERIOD), first, "half a millisecond is not worth a move");
        second(&mut s, 5_000, 300);
        assert!(s.offset(MAX, PERIOD) > first, "the band reached the take");
    }

    /// No point clears a band nearly a refresh wide: the take stays.
    #[test]
    fn a_band_too_wide_leaves_the_take() {
        let mut s = Steer::new();
        second(&mut s, 8_000, 7_500);
        assert_eq!(s.offset(MAX, PERIOD), MAX);
    }

    /// A band across the vblank reads as one band, not two halves.
    #[test]
    fn a_band_across_the_vblank_is_one_band() {
        let b = band(&[16_500, 16_600, 100, 200], 16_667).unwrap();
        assert!(b.width < 1_000, "{}", b.width);
        assert!(b.earliest >= 16_400, "{}", b.earliest);
    }
}
