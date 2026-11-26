//! Where the latency intent takes its picture against the band its pictures arrive in.
//!
//! Newest wins: a picture that lands just after the take waits a whole refresh, and the one after
//! it lands before the next take and replaces it unseen. The host's frame clock and the panel's
//! drift apart, so the arrivals walk through the refresh, about 0.8 ms a minute. While their band
//! straddles the take, pictures alternate between two vblanks and some are never shown.
//!
//! The take sits at the limit the pacing allows while the band is clear of it. When the band comes
//! within [`UNSAFE`] of the limit, the take moves [`GUARD`] before the band, so every picture goes
//! to the next refresh, and keeps that distance if the band walks earlier. It returns to the limit
//! once the band clears the limit by [`GUARD`]. With no room before the band, or a band too wide to
//! clear, the take stays at the limit.

use std::fmt::Write;
use std::time::{Duration, Instant};

/// Arrivals the band is read over: about two seconds at 60 pictures a second.
const ARRIVALS: usize = 128;
/// Arrivals between looks at the band: about a second.
const EVERY: usize = 60;
/// Room the take leaves before the band, and the clearance that returns it to the limit.
const GUARD: Duration = Duration::from_millis(2);
/// A take this close to the band moves: the band walks about 13 µs between looks.
const UNSAFE: Duration = Duration::from_millis(1);
/// Arrivals the band holds, in percent of the window; the rest are a late burst it lets go.
const BAND_PCT: usize = 98;

pub(crate) struct Steer {
    /// Each arrival's place in the refresh, µs after the vblank, the last [`ARRIVALS`].
    phases: [u32; ARRIVALS],
    n: usize,
    /// Arrivals since the last look.
    since: usize,
    /// The refresh the phases are read in, µs.
    period: u64,
    /// The band at the last look.
    band: Option<Band>,
    /// Where the take sits before a band on the limit, µs into the refresh; `None` at the limit.
    aside: Option<u64>,
    /// Moves since the last report.
    moves: u32,
}

/// A band of arrivals, in µs: where its earliest usual arrival sits, how wide it runs, its centre.
#[derive(Clone, Copy, Debug)]
struct Band {
    earliest: u64,
    width: u64,
    centre: u64,
}

impl Band {
    /// How far `t` sits from the band, before or after it, in a `p` µs refresh; 0 inside it.
    fn gap(&self, t: u64, p: u64) -> u64 {
        let past = (t % p + p - self.earliest) % p;
        if past <= self.width {
            0
        } else {
            (past - self.width).min(p - past)
        }
    }
}

impl Steer {
    pub fn new() -> Self {
        Self {
            phases: [0; ARRIVALS],
            n: 0,
            since: 0,
            period: 0,
            band: None,
            aside: None,
            moves: 0,
        }
    }

    /// Forgets the arrivals: a new stream, a new host clock.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Notes `arrivals` against the vblank at `vblank`, `period` apart, and reads the band again
    /// every [`EVERY`] of them.
    pub fn note(&mut self, vblank: Instant, period: Duration, arrivals: &[Instant]) {
        let p = us(period);
        if p == 0 {
            return;
        }
        self.period = p;
        for &a in arrivals {
            let off = if a >= vblank {
                us(a - vblank) as i64
            } else {
                -(us(vblank - a) as i64)
            };
            self.phases[self.n % ARRIVALS] = off.rem_euclid(p as i64) as u32;
            self.n += 1;
            self.since += 1;
        }
        if self.since >= EVERY {
            self.since = 0;
            self.band = band(&self.phases[..self.n.min(ARRIVALS)], p);
        }
    }

    /// Where to take the picture when `limit` is the latest the take may sit.
    pub fn offset(&mut self, limit: Duration) -> Duration {
        let aside = self.band.and_then(|b| self.place(b, us(limit)));
        if aside != self.aside {
            self.moves += 1;
            self.aside = aside;
        }
        aside.map_or(limit, Duration::from_micros)
    }

    /// The take for `band` against `limit`, from where it sits now: `None` at the limit.
    fn place(&self, band: Band, limit: u64) -> Option<u64> {
        let p = self.period;
        let (guard, unsafe_) = (us(GUARD), us(UNSAFE));
        if band.width + 2 * guard >= p {
            return None;
        }
        // The latest point the guard's distance before the band, if the limit is past it.
        let before = || band.earliest.checked_sub(guard).filter(|&t| t < limit);
        match self.aside {
            Some(t) if t <= limit && band.gap(limit, p) < guard => {
                if band.gap(t, p) < unsafe_ {
                    before()
                } else {
                    Some(t)
                }
            }
            _ if band.gap(limit, p) < unsafe_ => before(),
            _ => None,
        }
    }

    /// `arrivals at … ms in a … ms band`, and when `steered` where the take sits and its moves,
    /// then a fresh count; `None` before the first look.
    pub fn summary(&mut self, steered: bool) -> Option<String> {
        let b = self.band?;
        let ms = |us: u64| us as f64 / 1000.0;
        let mut line = format!("arrivals at {:.1} ms in a {:.1} ms band", ms(b.centre), ms(b.width));
        if steered {
            let _ = match self.aside {
                Some(t) => write!(line, ", take before it at {:.1} ms", ms(t)),
                None => write!(line, ", take at the limit"),
            };
            let _ = write!(line, ", {} moves", std::mem::take(&mut self.moves));
        }
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
    const P: u64 = 16_666;
    const LIMIT: Duration = Duration::from_millis(10);

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

    /// Enough seconds at one place that the band holds no older arrival.
    fn settle(steer: &mut Steer, centre_us: u64, jitter_us: u64) {
        for _ in 0..3 {
            second(steer, centre_us, jitter_us);
        }
    }

    /// A band clear of the limit, before or after it, leaves the take there.
    #[test]
    fn the_take_stays_at_the_limit_while_the_band_is_clear() {
        let mut s = Steer::new();
        assert_eq!(s.offset(LIMIT), LIMIT);
        settle(&mut s, 3_000, 500);
        assert_eq!(s.offset(LIMIT), LIMIT);
        settle(&mut s, 13_500, 500);
        assert_eq!(s.offset(LIMIT), LIMIT);
        assert_eq!(s.moves, 0);
    }

    /// A band on the limit moves the take the guard's distance before it; the take returns once
    /// the band clears the limit by the guard, and not before.
    #[test]
    fn a_band_on_the_limit_moves_the_take_before_it() {
        let mut s = Steer::new();
        settle(&mut s, 10_000, 300);
        let at = us(s.offset(LIMIT));
        assert!((7_600..=7_800).contains(&at), "{at}");
        settle(&mut s, 11_500, 300);
        assert_eq!(us(s.offset(LIMIT)), at, "1.2 ms past the limit is inside the guard");
        settle(&mut s, 12_500, 300);
        assert_eq!(s.offset(LIMIT), LIMIT);
        assert_eq!(s.moves, 2);
    }

    /// A band that walks through the refresh, earlier or later, never comes within half the
    /// unsafe distance of the take, and the take spends most of the walk at the limit.
    #[test]
    fn a_drifting_band_never_ends_up_on_the_take() {
        for step in [-13, 13] {
            drift(step);
        }
    }

    /// Twenty-five minutes of a band ±800 µs walking `step` µs a second, over a refresh.
    fn drift(step: i64) {
        let mut s = Steer::new();
        let mut centre: i64 = 8_000;
        let (mut moves, mut at_limit, mut last) = (0, 0, None);
        for _ in 0..1_500 {
            let c = centre.rem_euclid(P as i64) as u64;
            second(&mut s, c, 800);
            let at = us(s.offset(LIMIT));
            moves += u32::from(last.is_some_and(|l| l != at));
            at_limit += u32::from(at == us(LIMIT));
            last = Some(at);
            let all = Band {
                earliest: (c + P - 800) % P,
                width: 1_600,
                centre: c,
            };
            assert!(all.gap(at, P) >= us(UNSAFE) / 2, "take {at} µs, arrivals {c} ± 800 µs");
            centre += step;
        }
        assert!(moves > 0 && moves < 60, "{moves} moves at {step} µs a second");
        assert!(at_limit > 750, "{at_limit} of 1500 s at the limit");
    }

    /// No point clears a band nearly a refresh wide: the take stays at the limit.
    #[test]
    fn a_band_too_wide_leaves_the_take_at_the_limit() {
        let mut s = Steer::new();
        settle(&mut s, 8_000, 7_500);
        assert_eq!(s.offset(LIMIT), LIMIT);
    }

    /// A band across the vblank reads as one band, not two halves.
    #[test]
    fn a_band_across_the_vblank_is_one_band() {
        let b = band(&[16_500, 16_600, 100, 200], 16_667).unwrap();
        assert!(b.width < 1_000, "{}", b.width);
        assert!(b.earliest >= 16_400, "{}", b.earliest);
    }
}
