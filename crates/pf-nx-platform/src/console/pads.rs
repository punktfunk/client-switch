//! `Pads` over hid: four player slots, the handheld Joy-Cons merged into the first, HD rumble
//! on every pad's pair of actuators.

use pf_nx_app::platform::{HandheldGyro, Motion, PadFeedback, PadKind, PadSample, Pads, TouchPoint};
use pf_nx_sys::hid::{self, Style, Vibration};

use crate::pad_map;

const PLAYERS: u8 = 4;

pub struct NxPads {
    pads: hid::Pads,
    /// Slots the host has rumbled since the last [`Pads::release_all`]: the log names the first
    /// rumble of each, so a still pad tells "nothing asked" from "asked and not felt".
    rumbled: u8,
    /// hid's fingers, reused across polls.
    fingers: Vec<hid::Touch>,
    /// The console sensor failed once and is not asked again.
    console_failed: bool,
    /// Slot 0's motion came from the console sensor on its last read.
    console_sent: bool,
    gyro: HandheldGyro,
}

impl NxPads {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            pads: hid::Pads::init(PLAYERS)?,
            rumbled: 0,
            fingers: Vec::new(),
            console_failed: false,
            console_sent: false,
            gyro: HandheldGyro::default(),
        })
    }

    /// The slot's pad sensor: a Joy-Con pair's right Joy-Con, handheld too.
    fn joy_con(&mut self, slot: u8) -> Option<Motion> {
        let s = self.pads.sixaxis(usize::from(slot))?;
        let (gyro, accel) = pad_map::motion(s.accel, s.gyro);
        Some(Motion { gyro, accel })
    }

    /// The console's own sensor while handheld. Its ten floats are accel, gyro and a
    /// quaternion, in the pad sensor's frame and units (build dea5632's log beside the
    /// Joy-Con's).
    fn console(&mut self) -> Option<Motion> {
        if self.console_failed {
            return None;
        }
        let c = match self.pads.console_sensor() {
            Ok(c) => c?,
            Err(e) => {
                self.console_failed = true;
                tracing::info!("console sensor: {e:#}; handheld motion stays on the Joy-Con");
                return None;
            }
        };
        let [a0, a1, a2, g0, g1, g2, ..] = c.floats;
        let (gyro, accel) = pad_map::motion([a0, a1, a2], [g0, g1, g2]);
        Some(Motion { gyro, accel })
    }

    fn rumble(&mut self, slot: u8, low: u16, high: u16) {
        let (amp_low, amp_high) = pad_map::rumble(low, high);
        let v = Vibration {
            amp_low,
            freq_low: pad_map::RUMBLE_HZ.0,
            amp_high,
            freq_high: pad_map::RUMBLE_HZ.1,
        };
        if let Err(e) = self.pads.send_vibration(usize::from(slot), [v, v]) {
            tracing::debug!("pad {slot}: rumble: {e:#}");
        }
    }
}

/// The pad the player holds, by its actual type (plan D9).
fn kind(style: Style) -> PadKind {
    match style {
        Style::Handheld | Style::JoyDual => PadKind::JoyConPair,
        Style::JoyLeft => PadKind::JoyConLeft,
        Style::JoyRight => PadKind::JoyConRight,
        Style::FullKey => PadKind::SwitchPro,
        Style::Other => PadKind::Other,
    }
}

impl Pads for NxPads {
    fn poll(&mut self, out: &mut Vec<PadSample>) {
        self.pads.update();
        for slot in 0..PLAYERS {
            let Some(raw) = self.pads.sample(usize::from(slot)) else {
                continue;
            };
            if !raw.connected {
                continue;
            }
            let (lt, rt) = pad_map::triggers(raw.buttons);
            out.push(PadSample {
                slot,
                kind: kind(raw.style),
                buttons: pad_map::buttons(raw.buttons),
                lx: pad_map::stick(raw.lx),
                ly: pad_map::stick(raw.ly),
                rx: pad_map::stick(raw.rx),
                ry: pad_map::stick(raw.ry),
                lt,
                rt,
            });
        }
    }

    fn feedback(&mut self, slot: u8, feedback: &PadFeedback) {
        let &PadFeedback::Rumble { low, high } = feedback;
        let bit = 1u8.checked_shl(u32::from(slot)).unwrap_or(0);
        if (low, high) != (0, 0) && self.rumbled & bit == 0 {
            self.rumbled |= bit;
            tracing::info!("pad {slot}: the host rumbles it, low {low} high {high}");
        }
        self.rumble(slot, low, high);
    }

    fn release_all(&mut self) {
        self.rumbled = 0;
        for slot in 0..PLAYERS {
            self.rumble(slot, 0, 0);
        }
    }

    /// Handheld, slot 0 is the console's own sensor unless [`HandheldGyro::Controller`] is
    /// set: the attached right Joy-Con sits at the console's edge and read 0.92–0.99 g and
    /// 10–20% over gravity's turn, where the console read 1.02 g and turned true (builds
    /// dea5632 and a264034).
    fn motion(&mut self, slot: u8) -> Option<Motion> {
        if slot == 0 && self.gyro == HandheldGyro::Console {
            let console = self.console();
            if console.is_some() != self.console_sent {
                self.console_sent = console.is_some();
                if self.console_sent {
                    tracing::info!("pad 0: handheld, the console's own sensor goes out");
                }
            }
            if console.is_some() {
                return console;
            }
        }
        self.joy_con(slot)
    }

    fn set_handheld_gyro(&mut self, gyro: HandheldGyro) {
        self.gyro = gyro;
        self.console_sent = false;
    }

    fn touches(&mut self, out: &mut Vec<TouchPoint>) {
        hid::touches(&mut self.fingers);
        out.clear();
        out.extend(self.fingers.iter().map(|f| {
            let (x, y) = pad_map::touch(f.x, f.y);
            TouchPoint { id: f.id, x, y }
        }));
    }
}
