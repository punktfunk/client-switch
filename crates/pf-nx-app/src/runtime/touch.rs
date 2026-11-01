//! The console's screen as a touch screen on the host: fingers in, `Touch*` events out, placed on
//! the stream's picture. Pure: fingers in, events out.

use punktfunk_core::input::{InputEvent, InputKind};
use punktfunk_core::video_fit::{self, VideoFit};

use crate::platform::TouchPoint;

/// Where the picture sits: the output it is drawn in and the stream frame, both in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Screen {
    pub output: (u32, u32),
    pub frame: (u32, u32),
}

impl Screen {
    /// A finger, across the screen, in frame pixels. Off the picture it lands on its edge.
    fn to_frame(self, x: f32, y: f32) -> (i32, i32) {
        let place = video_fit::place(VideoFit::Fit, self.output, self.frame);
        let (fx, fy) = place.to_frame(
            f64::from(x) * f64::from(self.output.0),
            f64::from(y) * f64::from(self.output.1),
        );
        let last = |n: u32| f64::from(n.saturating_sub(1));
        (
            fx.round().min(last(self.frame.0)) as i32,
            fy.round().min(last(self.frame.1)) as i32,
        )
    }

    /// The frame size as the touch events carry it.
    fn flags(self) -> u32 {
        ((self.frame.0 & 0xffff) << 16) | (self.frame.1 & 0xffff)
    }
}

/// A finger the host holds: the console's id, the host's slot, where it was last sent.
struct Held {
    id: u32,
    slot: u32,
    at: (i32, i32),
}

/// The fingers the host holds, by the compact slots the wire reuses after an up.
#[derive(Default)]
pub struct TouchRouter {
    held: Vec<Held>,
}

impl TouchRouter {
    /// Diffs the fingers down now against the last step: ups first, then downs and moves.
    pub fn step(&mut self, fingers: &[TouchPoint], screen: Screen, wire: &mut Vec<InputEvent>) {
        self.held.retain(|h| {
            let stays = fingers.iter().any(|f| f.id == h.id);
            if !stays {
                wire.push(up(h.slot));
            }
            stays
        });
        if screen.frame.0 == 0 || screen.frame.1 == 0 {
            return;
        }
        for f in fingers {
            let at = screen.to_frame(f.x, f.y);
            if let Some(h) = self.held.iter_mut().find(|h| h.id == f.id) {
                if h.at != at {
                    h.at = at;
                    wire.push(event(InputKind::TouchMove, h.slot, at, screen.flags()));
                }
                continue;
            }
            let slot = (0..).find(|s| self.held.iter().all(|h| h.slot != *s)).unwrap_or(0);
            self.held.push(Held { id: f.id, slot, at });
            wire.push(event(InputKind::TouchDown, slot, at, screen.flags()));
        }
    }

    /// Lifts every finger the host holds.
    pub fn release_all(&mut self, wire: &mut Vec<InputEvent>) {
        wire.extend(self.held.drain(..).map(|h| up(h.slot)));
    }
}

fn event(kind: InputKind, slot: u32, (x, y): (i32, i32), flags: u32) -> InputEvent {
    InputEvent {
        kind,
        _pad: [0; 3],
        code: slot,
        x,
        y,
        flags,
    }
}

fn up(slot: u32) -> InputEvent {
    event(InputKind::TouchUp, slot, (0, 0), 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HANDHELD: Screen = Screen {
        output: (1280, 720),
        frame: (1280, 720),
    };

    fn finger(id: u32, x: f32, y: f32) -> TouchPoint {
        TouchPoint { id, x, y }
    }

    fn kinds(wire: &[InputEvent]) -> Vec<(InputKind, u32, i32, i32)> {
        wire.iter().map(|e| (e.kind, e.code, e.x, e.y)).collect()
    }

    #[test]
    fn a_tap_goes_down_and_up_on_the_picture() {
        let mut r = TouchRouter::default();
        let mut wire = Vec::new();
        r.step(&[finger(7, 0.5, 0.25)], HANDHELD, &mut wire);
        r.step(&[finger(7, 0.5, 0.25)], HANDHELD, &mut wire);
        r.step(&[], HANDHELD, &mut wire);
        assert_eq!(
            kinds(&wire),
            [(InputKind::TouchDown, 0, 640, 180), (InputKind::TouchUp, 0, 0, 0)]
        );
        assert_eq!(wire[0].flags, (1280 << 16) | 720);
    }

    #[test]
    fn a_second_finger_takes_the_next_slot_and_slots_are_reused() {
        let mut r = TouchRouter::default();
        let mut wire = Vec::new();
        r.step(&[finger(3, 0.0, 0.0), finger(9, 1.0, 1.0)], HANDHELD, &mut wire);
        r.step(&[finger(9, 0.5, 0.5)], HANDHELD, &mut wire);
        r.step(&[finger(9, 0.5, 0.5), finger(4, 0.25, 0.5)], HANDHELD, &mut wire);
        assert_eq!(
            kinds(&wire),
            [
                (InputKind::TouchDown, 0, 0, 0),
                (InputKind::TouchDown, 1, 1279, 719),
                (InputKind::TouchUp, 0, 0, 0),
                (InputKind::TouchMove, 1, 640, 360),
                (InputKind::TouchDown, 0, 320, 360),
            ]
        );
    }

    #[test]
    fn a_smaller_frame_scales_and_bars_clamp() {
        let mut r = TouchRouter::default();
        let mut wire = Vec::new();
        // A 4:3 frame on the 16:9 screen: bars left and right, the middle is the frame's.
        let screen = Screen {
            output: (1280, 720),
            frame: (960, 720),
        };
        r.step(&[finger(1, 0.5, 0.5), finger(2, 0.0, 0.5)], screen, &mut wire);
        assert_eq!(
            kinds(&wire),
            [(InputKind::TouchDown, 0, 480, 360), (InputKind::TouchDown, 1, 0, 360)]
        );
    }

    #[test]
    fn release_lifts_everything() {
        let mut r = TouchRouter::default();
        let mut wire = Vec::new();
        r.step(&[finger(1, 0.5, 0.5), finger(2, 0.1, 0.1)], HANDHELD, &mut wire);
        wire.clear();
        r.release_all(&mut wire);
        assert_eq!(
            kinds(&wire),
            [(InputKind::TouchUp, 0, 0, 0), (InputKind::TouchUp, 1, 0, 0)]
        );
        wire.clear();
        r.release_all(&mut wire);
        assert!(wire.is_empty());
    }
}
