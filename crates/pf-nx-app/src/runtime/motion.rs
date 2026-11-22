//! Each pad's gyro and accelerometer onto its pad on the host, in the wire's units. Pure: samples
//! in, rich input out, one call per pad per input tick.
//!
//! The host holds the last rate it got and stamps its own clock on the reports a game reads, so
//! a moving pad sends every other tick and the turn stays the same. A pad at rest sends nothing.
//!
//! Every 10 s a moving pad logs, per wire axis, the turn it read and what the host was given
//! (what the sending loses), the mean gravity (how the pad was held), and a gravity check: between
//! two steady poses, the turn gravity sees against the gyro's. Each axis gravity saw turn gives
//! the gyro's response to it, `(1.00, 0.00, 0.00)` for x when hid's unit and axes are right.

use std::time::{Duration, Instant};

use punktfunk_core::input::gamepad::{MOTION_ACCEL_LSB_PER_G, MOTION_GYRO_LSB_PER_DEG_S};
use punktfunk_core::quic::RichInput;

use super::pads::SLOTS;
use crate::platform::Motion;

/// The wire's gyro counts per rad/s.
const GYRO_LSB_PER_RAD_S: f32 = MOTION_GYRO_LSB_PER_DEG_S as f32 * 180.0 / std::f32::consts::PI;
/// 0.5 °/s: over a resting gyro's noise (about 0.1 °/s; hid reads exactly 0 on a table), under
/// slow deliberate aim (a few °/s).
const STILL_GYRO: u16 = (MOTION_GYRO_LSB_PER_DEG_S / 2) as u16;
/// Still samples in a row that put a pad at rest: 100 ms at 250 Hz, longer than a reversal.
const REST_SAMPLES: u16 = 25;
/// 0.02 g, about 1° of tilt: a resting accelerometer jitters by a few thousandths of a g.
const REST_ACCEL: u16 = (MOTION_ACCEL_LSB_PER_G / 50) as u16;
/// A tick longer than this (a stall, the dial) counts as this long in the summary.
const LONGEST_TICK: Duration = Duration::from_millis(50);
const SUMMARY_EVERY: Duration = Duration::from_secs(10);
/// A window that turned less than this is not logged.
const SUMMARY_MIN_DEG: f32 = 5.0;
/// A steady pose: under 10 °/s (a held hand's tremor is a few) and gravity within 8% of 1 g,
/// so the accelerometer reads mostly gravity and not a swing's push.
const STEADY_RAD_S: f32 = 10.0 * std::f32::consts::PI / 180.0;
const STEADY_G: f32 = 0.08;
/// 40 ms of steady samples in a row make a pose.
const STEADY_TICKS: u16 = 10;
/// A turn between two poses the check counts: big enough that a degree of noise is a few
/// percent. The same floor, squared, on an axis's share before its response is logged.
const CHECK_MIN_RAD: f32 = 10.0 * std::f32::consts::PI / 180.0;

type V3 = [f32; 3];

fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn norm(a: V3) -> f32 {
    dot(a, a).sqrt()
}

/// A sample in the wire's counts, `(gyro, accel)`.
type Counts = ([i16; 3], [i16; 3]);

fn counts(m: &Motion) -> Counts {
    let scale = |v: V3, per: f32| v.map(|c| (c * per).clamp(-32768.0, 32767.0) as i16);
    (
        scale(m.gyro, GYRO_LSB_PER_RAD_S),
        scale(m.accel, MOTION_ACCEL_LSB_PER_G as f32),
    )
}

/// The pad's turn from pose `up0` to pose `up1` as gravity sees it, a rotation vector in radians
/// in the pad's frame: the share across gravity only, since a turn about it moves nothing.
fn turn_by_gravity(up0: V3, up1: V3) -> Option<V3> {
    let (a, b) = (up0.map(|c| c / norm(up0)), up1.map(|c| c / norm(up1)));
    let c = cross(a, b);
    let s = norm(c);
    let angle = s.atan2(dot(a, b));
    // The pad turns one way, gravity seen from it the other.
    (angle >= CHECK_MIN_RAD).then(|| c.map(|v| -v / s * angle))
}

/// One pad's motion over a summary window. Angles in degrees, per wire axis.
#[derive(Clone, Copy, Default)]
struct Sums {
    time: Duration,
    ticks: u32,
    sends: u32,
    /// Total turn either way.
    travel: V3,
    /// Net turn read off the pad.
    read: V3,
    /// Net turn the host was given: each tick at the rate it holds.
    given: V3,
    /// Gravity summed over the ticks, and its length summed.
    accel: V3,
    g: f32,
    /// Ticks under [`STEADY_RAD_S`], ticks within [`STEADY_G`] of 1 g.
    calm: u32,
    one_g: u32,
    /// The gravity check: per axis gravity saw turn about, the gyro's turn weighted by it, and
    /// its weight; their ratio is the gyro's response to that axis.
    response: [V3; 3],
    weight: V3,
    pairs: u32,
}

impl Sums {
    fn add(&mut self, dt: Duration, m: &Motion, held: [i16; 3], sent: bool) {
        let s = dt.as_secs_f32();
        let per = MOTION_GYRO_LSB_PER_DEG_S as f32;
        for i in 0..3 {
            let r = m.gyro[i].to_degrees() * s;
            self.travel[i] += r.abs();
            self.read[i] += r;
            self.given[i] += f32::from(held[i]) / per * s;
            self.accel[i] += m.accel[i];
        }
        let g = norm(m.accel);
        self.g += g;
        self.calm += u32::from(norm(m.gyro) < STEADY_RAD_S);
        self.one_g += u32::from((g - 1.0).abs() < STEADY_G);
        self.time += dt;
        self.ticks += 1;
        self.sends += u32::from(sent);
    }

    fn pair(&mut self, by_gravity: V3, by_gyro: V3) {
        for (axis, w) in by_gravity.into_iter().enumerate() {
            for (r, g) in self.response[axis].iter_mut().zip(by_gyro) {
                *r += g * w;
            }
            self.weight[axis] += w * w;
        }
        self.pairs += 1;
    }

    /// The gyro's response to a turn about `axis`, once gravity saw enough of one.
    fn response(&self, axis: usize) -> Option<V3> {
        let w = self.weight[axis];
        (w >= CHECK_MIN_RAD * CHECK_MIN_RAD).then(|| self.response[axis].map(|r| r / w))
    }

    fn log(&self, name: &str, pad: u8) {
        if self.travel.iter().all(|t| *t < SUMMARY_MIN_DEG) {
            return;
        }
        let n = self.ticks.max(1) as f32;
        let pct = |c: u32| c as f32 * 100.0 / n;
        let [tx, ty, tz] = self.travel;
        let [rx, ry, rz] = self.read;
        let [lx, ly, lz] = std::array::from_fn::<f32, 3, _>(|i| self.given[i] - self.read[i]);
        let [ax, ay, az] = self.accel.map(|a| a / n);
        let check = (0..3)
            .map(|axis| match self.response(axis) {
                Some([x, y, z]) => format!("{}→({x:.2}, {y:.2}, {z:.2})", ["x", "y", "z"][axis]),
                None => format!("{}→—", ["x", "y", "z"][axis]),
            })
            .collect::<Vec<_>>()
            .join(" ");
        tracing::info!(
            "{name} {pad}: gyro over {:.1}s; turned x {tx:.0}° y {ty:.0}° z {tz:.0}°; net x {rx:+.1}° y {ry:+.1}° \
             z {rz:+.1}°, lost x {lx:+.1}° y {ly:+.1}° z {lz:+.1}°; gravity ({ax:+.2}, {ay:+.2}, {az:+.2}) \
             |g| {:.3}; calm {:.0}% 1g {:.0}%; {} poses paired, {check}; {} of {} ticks sent",
            self.time.as_secs_f32(),
            self.g / n,
            pct(self.calm),
            pct(self.one_g),
            self.pairs,
            self.sends,
            self.ticks,
        );
    }
}

/// The gravity check's running state: the gyro integrated, and the last steady pose.
#[derive(Clone, Copy, Default)]
struct GravityCheck {
    /// Radians about each axis since the pad came, by the gyro.
    turned: V3,
    steady: u16,
    /// `(gravity, turned)` at the last steady sample.
    pose: Option<(V3, V3)>,
}

impl GravityCheck {
    /// Pairs each new pose with the one before it: a turn over [`CHECK_MIN_RAD`] across gravity
    /// goes into `sums`, the gyro's turn taken across gravity the same way.
    fn step(&mut self, m: &Motion, dt: Duration, sums: &mut Sums) {
        let s = dt.as_secs_f32();
        for (t, w) in self.turned.iter_mut().zip(m.gyro) {
            *t += w * s;
        }
        if norm(m.gyro) >= STEADY_RAD_S || (norm(m.accel) - 1.0).abs() >= STEADY_G {
            self.steady = 0;
            return;
        }
        self.steady = self.steady.saturating_add(1);
        if self.steady < STEADY_TICKS {
            return;
        }
        if let (STEADY_TICKS, Some((up0, t0))) = (self.steady, self.pose) {
            if let Some(by_gravity) = turn_by_gravity(up0, m.accel) {
                let mid = std::array::from_fn(|i| up0[i] / norm(up0) + m.accel[i] / norm(m.accel));
                let mid = mid.map(|c: f32| c / norm(mid));
                let by_gyro: V3 = std::array::from_fn(|i| self.turned[i] - t0[i]);
                let along = dot(by_gyro, mid);
                sums.pair(by_gravity, std::array::from_fn(|i| by_gyro[i] - mid[i] * along));
            }
        }
        self.pose = Some((m.accel, self.turned));
    }
}

/// One pad's motion as the host holds it.
#[derive(Clone, Copy, Default)]
struct PadMotion {
    sent: Option<Counts>,
    /// Samples in a row whose gyro read under [`STILL_GYRO`] on every axis.
    still: u16,
    /// The previous tick sent, so this one stays home.
    just_sent: bool,
    last_tick: Option<Instant>,
    sums: Sums,
    check: GravityCheck,
}

pub struct MotionRouter {
    pads: [PadMotion; SLOTS],
    /// What the log calls each source: `pad`, or `joy-con` for pad 0's shadow.
    name: &'static str,
}

impl Default for MotionRouter {
    fn default() -> Self {
        Self::named("pad")
    }
}

impl MotionRouter {
    /// A router whose log lines read `{name} {pad}: …`.
    pub fn named(name: &'static str) -> Self {
        Self {
            pads: Default::default(),
            name,
        }
    }

    /// One pad's latest sample at `now`, `None` when the pad is gone: the rich input to send, if
    /// any. A changed sample goes out unless the previous tick sent. After [`REST_SAMPLES`] still
    /// samples the pad sends one zero rate and then only a gravity shift over [`REST_ACCEL`];
    /// one sample over [`STILL_GYRO`] makes it live again. A pad's first sample is logged, so a
    /// run shows the frame hid reports in.
    pub fn step(&mut self, pad: u8, sample: Option<Motion>, now: Instant) -> Option<RichInput> {
        let p = self.pads.get_mut(usize::from(pad))?;
        let Some(m) = sample else {
            *p = PadMotion::default();
            return None;
        };
        let (gyro, accel) = counts(&m);
        p.still = if gyro.iter().all(|c| c.unsigned_abs() <= STILL_GYRO) {
            p.still.saturating_add(1)
        } else {
            0
        };
        let next = match p.sent {
            None => {
                let name = self.name;
                tracing::info!("{name} {pad}: motion, gyro {:?} rad/s, accel {:?} g", m.gyro, m.accel);
                (gyro, accel)
            }
            Some((_, held)) if p.still >= REST_SAMPLES => {
                let shifted = accel.iter().zip(held).any(|(a, h)| a.abs_diff(h) > REST_ACCEL);
                ([0; 3], if shifted { accel } else { held })
            }
            Some(_) => (gyro, accel),
        };
        let send = !std::mem::take(&mut p.just_sent) && p.sent != Some(next);
        if send {
            p.sent = Some(next);
            p.just_sent = true;
        }
        let dt = p
            .last_tick
            .map_or(Duration::ZERO, |t| now.saturating_duration_since(t).min(LONGEST_TICK));
        p.last_tick = Some(now);
        p.sums.add(dt, &m, p.sent.map_or([0; 3], |(g, _)| g), send);
        p.check.step(&m, dt, &mut p.sums);
        if p.sums.time >= SUMMARY_EVERY {
            p.sums.log(self.name, pad);
            p.sums = Sums::default();
        }
        send.then_some(RichInput::Motion {
            pad,
            gyro: next.0,
            accel: next.1,
        })
    }

    /// Stops every pad turning on the host, gravity kept: the host holds the last rate it got.
    /// Each pad's next sample may go out at once.
    pub fn still_all(&mut self, out: &mut Vec<RichInput>) {
        for (pad, p) in self.pads.iter_mut().enumerate() {
            p.just_sent = false;
            if let Some((gyro, accel)) = &mut p.sent {
                if *gyro != [0; 3] {
                    *gyro = [0; 3];
                    out.push(RichInput::Motion {
                        pad: pad as u8,
                        gyro: [0; 3],
                        accel: *accel,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: Duration = Duration::from_millis(4);

    fn still() -> Motion {
        Motion {
            gyro: [0.0; 3],
            accel: [0.0, 1.0, 0.0],
        }
    }

    /// Turning at `deg_s` about x, gravity on y.
    fn turning(deg_s: f32) -> Motion {
        Motion {
            gyro: [deg_s.to_radians(), 0.0, 0.0],
            ..still()
        }
    }

    /// A router with its own clock, one tick per call.
    struct Rig {
        r: MotionRouter,
        now: Instant,
    }

    impl Rig {
        fn new() -> Self {
            Self {
                r: MotionRouter::default(),
                now: Instant::now(),
            }
        }

        fn step(&mut self, pad: u8, m: Option<Motion>) -> Option<RichInput> {
            self.now += TICK;
            self.r.step(pad, m, self.now)
        }

        /// Each tick's output for `samples` on pad 0.
        fn run(&mut self, samples: impl IntoIterator<Item = Motion>) -> Vec<Option<RichInput>> {
            samples.into_iter().map(|m| self.step(0, Some(m))).collect()
        }

        fn sums(&self) -> Sums {
            self.r.pads[0].sums
        }
    }

    fn gyro(out: &RichInput) -> [i16; 3] {
        let RichInput::Motion { gyro, .. } = out else {
            panic!("motion out");
        };
        *gyro
    }

    #[test]
    fn samples_go_out_in_the_wire_units_once_each() {
        let mut r = Rig::new();
        let turning = Motion {
            gyro: [std::f32::consts::PI, 0.0, -100.0],
            ..still()
        };
        let Some(RichInput::Motion { pad, gyro, accel }) = r.step(1, Some(turning)) else {
            panic!("a first sample goes out");
        };
        // π rad/s is 180 °/s, 20 counts each; past the range clamps.
        assert_eq!((pad, gyro, accel), (1, [3600, 0, -32768], [0, 10_000, 0]));
        assert!(r.step(1, Some(turning)).is_none(), "the next tick stays home");
        assert!(r.step(1, Some(turning)).is_none(), "an unchanged sample stays home");
    }

    #[test]
    fn a_moving_pad_sends_every_other_tick() {
        let mut r = Rig::new();
        let out = r.run((0..100).map(|i| turning(30.0 + i as f32)));
        assert!(out.iter().step_by(2).all(Option::is_some));
        assert!(out.iter().skip(1).step_by(2).all(Option::is_none));
    }

    #[test]
    fn slow_aim_stays_live() {
        let mut r = Rig::new();
        // 2 °/s drifting by a count a tick: 40 counts and up, never at rest.
        let out = r.run((0..200).map(|i| turning(2.0 + i as f32 * 0.05)));
        let sent: Vec<_> = out.iter().flatten().collect();
        assert_eq!(sent.len(), 100);
        assert!(sent.iter().all(|m| gyro(m)[0] >= 39));
    }

    #[test]
    fn a_resting_pad_sends_one_zero_then_nothing() {
        let mut r = Rig::new();
        // ±0.3 °/s and ±0.002 g of noise: still, though no two ticks read the same.
        let noisy = |i: usize| {
            let sign: f32 = if i % 2 == 0 { 1.0 } else { -1.0 };
            Motion {
                gyro: [0.0, 0.0, (0.3 * sign).to_radians()],
                accel: [0.0, 1.0 + 0.002 * sign, 0.0],
            }
        };
        let out = r.run((0..400).map(noisy));
        let (tick, last) = out
            .iter()
            .enumerate()
            .rev()
            .find_map(|(t, o)| Some((t, o.as_ref()?)))
            .expect("the first sample goes out");
        assert!(tick <= usize::from(REST_SAMPLES), "quiet from tick {tick}");
        assert_eq!(gyro(last), [0; 3], "the last rate the host holds is zero");

        assert!(r.step(0, Some(turning(1.0))).is_some(), "a turn wakes it at once");
    }

    #[test]
    fn a_resting_pad_sends_a_shift_of_gravity() {
        let mut r = Rig::new();
        r.run(std::iter::repeat_n(still(), 40));
        let small = Motion {
            accel: [0.0, 1.01, 0.0],
            ..still()
        };
        assert!(r.step(0, Some(small)).is_none(), "0.01 g stays home");
        let tilted = Motion {
            accel: [0.1, 0.99, 0.0],
            ..still()
        };
        let Some(RichInput::Motion { gyro, accel, .. }) = r.step(0, Some(tilted)) else {
            panic!("a shift of gravity goes out");
        };
        assert_eq!((gyro, accel), ([0; 3], [1000, 9900, 0]));
    }

    #[test]
    fn the_sums_match_when_nothing_is_lost() {
        let mut r = Rig::new();
        // 45° left, 90° right, 45° left at 90 °/s, then rest: back where it started.
        let swing = |deg_s: f32, ticks| {
            std::iter::repeat_n(
                Motion {
                    gyro: [0.0, deg_s.to_radians(), 0.0],
                    ..still()
                },
                ticks,
            )
        };
        r.run(
            std::iter::repeat_n(still(), 10)
                .chain(swing(90.0, 125))
                .chain(swing(-90.0, 250))
                .chain(swing(90.0, 125))
                .chain(std::iter::repeat_n(still(), 50)),
        );
        let s = r.sums();
        assert!((s.travel[1] - 180.0).abs() < 1.0, "travel {}", s.travel[1]);
        assert!(s.read[1].abs() < 0.1, "read {}", s.read[1]);
        // Each rate goes out a tick late and the host holds it a tick long: under a degree.
        assert!(s.given[1].abs() < 1.0, "given {}", s.given[1]);
    }

    /// `v` turned by `deg` about the unit `axis` (Rodrigues).
    fn rotate(v: V3, axis: V3, deg: f32) -> V3 {
        let (s, c) = deg.to_radians().sin_cos();
        let k = cross(axis, v);
        let d = dot(axis, v) * (1.0 - c);
        std::array::from_fn(|i| v[i] * c + k[i] * s + axis[i] * d)
    }

    /// Steady at gravity `up`, then turned by `deg` about the unit `axis` at 90 °/s while the gyro
    /// reads `gyro_of(rate vector)`, then steady again. Returns the samples and the gravity after.
    fn turn(up: V3, axis: V3, deg: f32, gyro_of: impl Fn(V3) -> V3) -> (Vec<Motion>, V3) {
        let steady = |up: V3| {
            std::iter::repeat_n(
                Motion {
                    gyro: [0.0; 3],
                    accel: up,
                },
                20,
            )
        };
        let ticks = (deg.abs() / 90.0 / TICK.as_secs_f32()).round() as usize;
        let rate = axis.map(|a| (90f32 * deg.signum() * a).to_radians());
        // Seen from the pad, gravity turns the other way.
        let moving = (1..=ticks).map(|i| Motion {
            gyro: gyro_of(rate),
            accel: rotate(up, axis, -deg * i as f32 / ticks as f32),
        });
        let end = rotate(up, axis, -deg);
        (steady(up).chain(moving).chain(steady(end)).collect(), end)
    }

    fn near(a: V3, b: V3) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.03)
    }

    #[test]
    fn gravity_sees_a_tilt_about_x_and_a_roll_about_z() {
        let level = [0.0, 1.0, 0.0];
        let tilt = turn_by_gravity(level, rotate(level, [1.0, 0.0, 0.0], -30.0)).unwrap();
        assert!(near(tilt, [30f32.to_radians(), 0.0, 0.0]), "{tilt:?}");
        let roll = turn_by_gravity(level, rotate(level, [0.0, 0.0, 1.0], -30.0)).unwrap();
        assert!(near(roll, [0.0, 0.0, 30f32.to_radians()]), "{roll:?}");
        assert!(turn_by_gravity(level, rotate(level, [1.0, 0.0, 0.0], 5.0)).is_none());
    }

    #[test]
    fn a_true_gyro_responds_one_to_one() {
        let mut r = Rig::new();
        let upright = [0.0, 0.0, -1.0];
        // Upright: up and down about x, then a wheel turn about y.
        let (a, up) = turn(upright, [1.0, 0.0, 0.0], 40.0, |w| w);
        let (b, up) = turn(up, [1.0, 0.0, 0.0], -40.0, |w| w);
        let (c, _) = turn(up, [0.0, 1.0, 0.0], 45.0, |w| w);
        r.run(a.into_iter().chain(b).chain(c));
        let s = r.sums();
        assert_eq!(s.pairs, 3);
        assert!(near(s.response(0).unwrap(), [1.0, 0.0, 0.0]), "{:?}", s.response(0));
        assert!(near(s.response(1).unwrap(), [0.0, 1.0, 0.0]), "{:?}", s.response(1));
        assert!(s.response(2).is_none(), "nothing turned about z across gravity");
    }

    #[test]
    fn a_weak_or_crossed_gyro_shows_in_its_response() {
        let mut r = Rig::new();
        // A gyro at 60% on x that also leaks a third of x onto y.
        let (a, _) = turn([0.0, 0.0, -1.0], [1.0, 0.0, 0.0], 45.0, |[x, y, z]| {
            [0.6 * x, y + 0.3 * x, z]
        });
        r.run(a);
        let k = r.sums().response(0).unwrap();
        // The leak's share along gravity is unseen, so y shows most but not all of it.
        assert!((k[0] - 0.6).abs() < 0.03 && k[1] > 0.2, "{k:?}");
    }

    #[test]
    fn a_swing_is_no_pose() {
        let mut r = Rig::new();
        // Gravity at 1.2 g: a push, not a pose, so nothing pairs.
        let push = |up: V3| up.map(|c| c * 1.2);
        let (a, _) = turn([0.0, 1.0, 0.0], [1.0, 0.0, 0.0], 45.0, |w| w);
        r.run(a.into_iter().map(|m| Motion {
            accel: push(m.accel),
            ..m
        }));
        assert_eq!(r.sums().pairs, 0);
    }

    #[test]
    fn still_all_stops_the_turning_and_keeps_gravity() {
        let mut r = Rig::new();
        let turning = Motion {
            gyro: [1.0, 0.0, 0.0],
            ..still()
        };
        r.step(0, Some(turning));
        r.step(2, Some(still()));
        let mut out = Vec::new();
        r.r.still_all(&mut out);
        assert_eq!(
            out,
            [RichInput::Motion {
                pad: 0,
                gyro: [0; 3],
                accel: [0, 10_000, 0]
            }]
        );
        out.clear();
        r.r.still_all(&mut out);
        assert!(out.is_empty());
        assert!(r.step(0, Some(turning)).is_some(), "turning again goes out");
    }

    #[test]
    fn a_pad_that_leaves_starts_over() {
        let mut r = Rig::new();
        r.step(0, Some(still()));
        assert!(r.step(0, None).is_none());
        assert!(r.step(0, Some(still())).is_some());
        assert!(r.step(9, Some(still())).is_none(), "no such slot");
    }
}
