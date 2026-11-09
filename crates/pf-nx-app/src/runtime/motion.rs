//! Each pad's gyro and accelerometer onto its pad on the host, in the wire's units. Pure: samples
//! in, rich input out, one call per pad per input tick.
//!
//! The host holds the last rate it got and stamps its own clock on the reports a game reads, so
//! a moving pad sends every other tick and the turn stays the same. A pad at rest sends nothing.

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

/// A sample in the wire's counts, `(gyro, accel)`.
type Counts = ([i16; 3], [i16; 3]);

fn counts(m: &Motion) -> Counts {
    let scale = |v: [f32; 3], per: f32| v.map(|c| (c * per).clamp(-32768.0, 32767.0) as i16);
    (
        scale(m.gyro, GYRO_LSB_PER_RAD_S),
        scale(m.accel, MOTION_ACCEL_LSB_PER_G as f32),
    )
}

/// One pad's motion as the host holds it.
#[derive(Clone, Copy, Default)]
struct PadMotion {
    sent: Option<Counts>,
    /// Samples in a row whose gyro read under [`STILL_GYRO`] on every axis.
    still: u16,
    /// The previous tick sent, so this one stays home.
    just_sent: bool,
}

#[derive(Default)]
pub struct MotionRouter {
    pads: [PadMotion; SLOTS],
}

impl MotionRouter {
    /// One pad's latest sample, `None` when the pad is gone: the rich input to send, if any.
    /// A changed sample goes out unless the previous tick sent. After [`REST_SAMPLES`] still
    /// samples the pad sends one zero rate and then only a gravity shift over [`REST_ACCEL`];
    /// one sample over [`STILL_GYRO`] makes it live again. A pad's first sample is logged, so a
    /// run shows the frame hid reports in.
    pub fn step(&mut self, pad: u8, sample: Option<Motion>) -> Option<RichInput> {
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
                tracing::info!("pad {pad}: motion, gyro {:?} rad/s, accel {:?} g", m.gyro, m.accel);
                (gyro, accel)
            }
            Some((_, held)) if p.still >= REST_SAMPLES => {
                let shifted = accel.iter().zip(held).any(|(a, h)| a.abs_diff(h) > REST_ACCEL);
                ([0; 3], if shifted { accel } else { held })
            }
            Some(_) => (gyro, accel),
        };
        if std::mem::take(&mut p.just_sent) || p.sent == Some(next) {
            return None;
        }
        p.sent = Some(next);
        p.just_sent = true;
        Some(RichInput::Motion {
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

    /// Each tick's output for `samples` on pad 0.
    fn run(r: &mut MotionRouter, samples: impl IntoIterator<Item = Motion>) -> Vec<Option<RichInput>> {
        samples.into_iter().map(|m| r.step(0, Some(m))).collect()
    }

    fn gyro(out: &RichInput) -> [i16; 3] {
        let RichInput::Motion { gyro, .. } = out else {
            panic!("motion out");
        };
        *gyro
    }

    #[test]
    fn samples_go_out_in_the_wire_units_once_each() {
        let mut r = MotionRouter::default();
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
        let mut r = MotionRouter::default();
        let out = run(&mut r, (0..100).map(|i| turning(30.0 + i as f32)));
        assert!(out.iter().step_by(2).all(Option::is_some));
        assert!(out.iter().skip(1).step_by(2).all(Option::is_none));
    }

    #[test]
    fn slow_aim_stays_live() {
        let mut r = MotionRouter::default();
        // 2 °/s drifting by a count a tick: 40 counts and up, never at rest.
        let out = run(&mut r, (0..200).map(|i| turning(2.0 + i as f32 * 0.05)));
        let sent: Vec<_> = out.iter().flatten().collect();
        assert_eq!(sent.len(), 100);
        assert!(sent.iter().all(|m| gyro(m)[0] >= 39));
    }

    #[test]
    fn a_resting_pad_sends_one_zero_then_nothing() {
        let mut r = MotionRouter::default();
        // ±0.3 °/s and ±0.002 g of noise: still, though no two ticks read the same.
        let noisy = |i: usize| {
            let sign: f32 = if i % 2 == 0 { 1.0 } else { -1.0 };
            Motion {
                gyro: [0.0, 0.0, (0.3 * sign).to_radians()],
                accel: [0.0, 1.0 + 0.002 * sign, 0.0],
            }
        };
        let out = run(&mut r, (0..400).map(noisy));
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
        let mut r = MotionRouter::default();
        run(&mut r, std::iter::repeat_n(still(), 40));
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
    fn still_all_stops_the_turning_and_keeps_gravity() {
        let mut r = MotionRouter::default();
        let turning = Motion {
            gyro: [1.0, 0.0, 0.0],
            ..still()
        };
        r.step(0, Some(turning));
        r.step(2, Some(still()));
        let mut out = Vec::new();
        r.still_all(&mut out);
        assert_eq!(
            out,
            [RichInput::Motion {
                pad: 0,
                gyro: [0; 3],
                accel: [0, 10_000, 0]
            }]
        );
        out.clear();
        r.still_all(&mut out);
        assert!(out.is_empty());
        assert!(r.step(0, Some(turning)).is_some(), "turning again goes out");
    }

    #[test]
    fn a_pad_that_leaves_starts_over() {
        let mut r = MotionRouter::default();
        r.step(0, Some(still()));
        assert!(r.step(0, None).is_none());
        assert!(r.step(0, Some(still())).is_some());
        assert!(r.step(9, Some(still())).is_none(), "no such slot");
    }
}
