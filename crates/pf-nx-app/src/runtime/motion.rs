//! Each pad's gyro and accelerometer onto its pad on the host, in the wire's units. Pure: samples
//! in, rich input out, one call per pad per input tick.
//!
//! The yaw sent is the player's: the turn about the vertical the accelerometer finds, so a pad
//! held upright (the console in handheld) turns the camera as far as one held level. Pitch,
//! roll and gravity stay the pad's own.
//!
//! The host holds the last rate it got and stamps its own clock on the reports a game reads, so
//! a moving pad sends every other tick and the turn stays the same. A pad at rest sends nothing.
//! Every 10 s a moving pad logs the angle it read against the angle the host was given.

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
/// Share of the way the held vertical moves to each accelerometer sample: about 200 ms at
/// 250 Hz, slow enough that a swing's own push barely tilts it.
const UP_FOLLOW: f32 = 0.02;
/// The Gyro Wiki's player-space relaxation: a turn about an axis tilted off the vertical still turns
/// in full, capped at the pad's whole yaw-and-roll rate.
const YAW_RELAX: f32 = 1.41;
/// A tick longer than this (a stall, the dial) counts as this long in the summary.
const LONGEST_TICK: Duration = Duration::from_millis(50);
const SUMMARY_EVERY: Duration = Duration::from_secs(10);
/// A window that turned less than this is not logged.
const SUMMARY_MIN_DEG: f32 = 5.0;

/// A sample in the wire's counts, `(gyro, accel)`.
type Counts = ([i16; 3], [i16; 3]);

fn counts(gyro: [f32; 3], accel: [f32; 3]) -> Counts {
    let scale = |v: [f32; 3], per: f32| v.map(|c| (c * per).clamp(-32768.0, 32767.0) as i16);
    (
        scale(gyro, GYRO_LSB_PER_RAD_S),
        scale(accel, MOTION_ACCEL_LSB_PER_G as f32),
    )
}

/// `gyro` with its yaw replaced by the turn about `up`, the vertical in the pad's frame (where
/// the table pushes). Gravity's share along x is left out, so aiming up or down keeps the yaw.
fn player_space(gyro: [f32; 3], up: [f32; 3]) -> [f32; 3] {
    let len = up.iter().map(|c| c * c).sum::<f32>().sqrt();
    if len < 0.5 {
        return gyro;
    }
    let [x, y, z] = gyro;
    let world = (y * up[1] + z * up[2]) / len;
    let yaw = world.signum() * (world.abs() * YAW_RELAX).min((y * y + z * z).sqrt());
    [x, yaw, z]
}

/// One pad's turn over a summary window, `[yaw, pitch]` in degrees.
#[derive(Clone, Copy, Default)]
struct Sums {
    time: Duration,
    ticks: u32,
    sends: u32,
    /// Total turn either way.
    travel: [f32; 2],
    /// Net turn read off the pad.
    read: [f32; 2],
    /// Net turn the host was given: each tick at the rate it holds.
    given: [f32; 2],
}

impl Sums {
    fn add(&mut self, dt: Duration, read: [f32; 3], held: [i16; 3], sent: bool) {
        let s = dt.as_secs_f32();
        let per = MOTION_GYRO_LSB_PER_DEG_S as f32;
        // [yaw, pitch] are the wire's y and x.
        for (i, axis) in [1, 0].into_iter().enumerate() {
            let r = read[axis].to_degrees() * s;
            self.travel[i] += r.abs();
            self.read[i] += r;
            self.given[i] += f32::from(held[axis]) / per * s;
        }
        self.time += dt;
        self.ticks += 1;
        self.sends += u32::from(sent);
    }

    fn log(&self, pad: u8) {
        let [ty, tp] = self.travel;
        if ty.max(tp) < SUMMARY_MIN_DEG {
            return;
        }
        let ([ry, rp], [gy, gp]) = (self.read, self.given);
        tracing::info!(
            "pad {pad}: gyro over {:.1}s, yaw {ty:.0}° turned, net {ry:+.1}° read {gy:+.1}° given ({:+.1}°); \
             pitch {tp:.0}° turned, net {rp:+.1}° read {gp:+.1}° given ({:+.1}°); {} of {} ticks sent",
            self.time.as_secs_f32(),
            gy - ry,
            gp - rp,
            self.sends,
            self.ticks,
        );
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
    /// The vertical in the pad's frame, the accelerometer smoothed.
    up: Option<[f32; 3]>,
    last_tick: Option<Instant>,
    sums: Sums,
}

#[derive(Default)]
pub struct MotionRouter {
    pads: [PadMotion; SLOTS],
}

impl MotionRouter {
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
        let up = match p.up {
            Some(up) => std::array::from_fn(|i| up[i] + (m.accel[i] - up[i]) * UP_FOLLOW),
            None => m.accel,
        };
        p.up = Some(up);
        let turn = player_space(m.gyro, up);
        let (gyro, accel) = counts(turn, m.accel);
        p.still = if gyro.iter().all(|c| c.unsigned_abs() <= STILL_GYRO) {
            p.still.saturating_add(1)
        } else {
            0
        };
        let next = match p.sent {
            None => {
                tracing::info!("pad {pad}: motion, gyro {:?} rad/s, accel {:?} g", m.gyro, m.accel);
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
        p.sums.add(dt, turn, p.sent.map_or([0; 3], |(g, _)| g), send);
        if p.sums.time >= SUMMARY_EVERY {
            p.sums.log(pad);
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
    fn a_level_pad_keeps_its_own_yaw() {
        let level = [0.0, 1.0, 0.0];
        assert_eq!(player_space([0.1, 0.5, 0.0], level), [0.1, 0.5, 0.0]);
        assert_eq!(player_space([0.1, -0.5, 0.0], level), [0.1, -0.5, 0.0]);
    }

    #[test]
    fn an_upright_pad_turns_as_far_as_a_level_one() {
        // Screen toward the player: the table pushes along the wire's -z, and a turn to the
        // left about the vertical reads on the pad's z.
        let upright = [0.0, 0.0, -1.0];
        assert_eq!(player_space([0.0, 0.0, -0.5], upright), [0.0, 0.5, -0.5]);
        assert_eq!(player_space([0.0, 0.0, 0.5], upright), [0.0, -0.5, 0.5]);
    }

    #[test]
    fn a_tilted_pad_turns_in_full() {
        // Tipped 60° back: a 1 rad/s turn about the vertical splits 0.5 on y, 0.87 on z.
        let (s, c) = 60f32.to_radians().sin_cos();
        let up = [0.0, c, -s];
        let [_, yaw, _] = player_space([0.0, c, -s], up);
        assert!((yaw - 1.0).abs() < 1e-5, "yaw {yaw}");
        // Pitch is the pad's own, whatever the tilt.
        assert_eq!(player_space([0.7, 0.0, 0.0], up)[..2], [0.7, 0.0]);
    }

    #[test]
    fn no_gravity_keeps_the_pad_frame() {
        assert_eq!(player_space([0.1, 0.2, 0.3], [0.0; 3]), [0.1, 0.2, 0.3]);
    }

    #[test]
    fn the_vertical_follows_the_pad_slowly() {
        let mut r = Rig::new();
        r.run(std::iter::repeat_n(still(), 10));
        // Stood upright at once and turning: the held vertical is still near level, so the
        // first ticks give little yaw, then the full turn once it has caught up.
        let upright_turn = Motion {
            gyro: [0.0, 0.0, -1.0],
            accel: [0.0, 0.0, -1.0],
        };
        let out = r.run(std::iter::repeat_n(upright_turn, 500));
        let first = gyro(out[0].as_ref().unwrap())[1];
        let last = out.iter().flatten().last().map(gyro).unwrap()[1];
        assert!(first < last / 2, "first {first}, last {last}");
        assert!(
            (i32::from(last) - 1146).abs() <= 2,
            "1 rad/s is 1146 counts, got {last}"
        );
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
        let s = r.r.pads[0].sums;
        assert!((s.travel[0] - 180.0).abs() < 1.0, "travel {}", s.travel[0]);
        assert!(s.read[0].abs() < 0.1, "read {}", s.read[0]);
        // Each rate goes out a tick late and the host holds it a tick long: under a degree.
        assert!(s.given[0].abs() < 1.0, "given {}", s.given[0]);
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
