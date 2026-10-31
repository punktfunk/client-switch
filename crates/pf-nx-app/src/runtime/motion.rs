//! Each pad's gyro and accelerometer onto its pad on the host, in the wire's units. Pure: samples
//! in, rich input out.

use punktfunk_core::input::gamepad::{MOTION_ACCEL_LSB_PER_G, MOTION_GYRO_LSB_PER_DEG_S};
use punktfunk_core::quic::RichInput;

use super::pads::SLOTS;
use crate::platform::Motion;

/// The wire's gyro counts per rad/s.
const GYRO_LSB_PER_RAD_S: f32 = MOTION_GYRO_LSB_PER_DEG_S as f32 * 180.0 / std::f32::consts::PI;

/// A sample in the wire's counts, `(gyro, accel)`.
type Counts = ([i16; 3], [i16; 3]);

fn counts(m: &Motion) -> Counts {
    let scale = |v: [f32; 3], per: f32| v.map(|c| (c * per).clamp(-32768.0, 32767.0) as i16);
    (
        scale(m.gyro, GYRO_LSB_PER_RAD_S),
        scale(m.accel, MOTION_ACCEL_LSB_PER_G as f32),
    )
}

/// What each pad last sent.
#[derive(Default)]
pub struct MotionRouter {
    sent: [Option<Counts>; SLOTS],
}

impl MotionRouter {
    /// One pad's latest sample, `None` when the pad is gone: the rich input to send when it
    /// changed. A pad's first sample is logged, so a run shows the frame hid reports in.
    pub fn step(&mut self, pad: u8, sample: Option<Motion>) -> Option<RichInput> {
        let slot = self.sent.get_mut(usize::from(pad))?;
        let Some(m) = sample else {
            *slot = None;
            return None;
        };
        let now = counts(&m);
        if *slot == Some(now) {
            return None;
        }
        if slot.is_none() {
            tracing::info!("pad {pad}: motion, gyro {:?} rad/s, accel {:?} g", m.gyro, m.accel);
        }
        *slot = Some(now);
        Some(RichInput::Motion {
            pad,
            gyro: now.0,
            accel: now.1,
        })
    }

    /// Stops every pad turning on the host, gravity kept: the host holds the last rate it got.
    pub fn still_all(&mut self, out: &mut Vec<RichInput>) {
        for (pad, slot) in self.sent.iter_mut().enumerate() {
            if let Some((gyro, accel)) = slot {
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
        assert!(r.step(1, Some(turning)).is_none(), "an unchanged sample stays home");
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
