//! Pad samples into what the loops act on: wire input for the host, menu events for the console,
//! the dial's own presses, and the hold-to-exit chord. Pure: samples in, events out.

use std::time::{Duration, Instant};

use pf_client_core::menu_nav::{MenuEvent, MenuSample};
use punktfunk_core::input::{gamepad, InputEvent, InputKind};

use crate::core::dial::{PadDial, PadRoute};
use crate::core::model::GamepadType;
use crate::platform::{PadKind, PadSample};

/// Slots the console reports: one per player.
pub const SLOTS: usize = 4;

/// The wire bits under the labels A, B, X and Y. The wire is positional and Nintendo's A sits
/// east, so it is `BTN_B`; X sits north. A confirms and B backs, as everywhere on the console.
pub const FACE: [u32; 4] = [gamepad::BTN_B, gamepad::BTN_A, gamepad::BTN_Y, gamepad::BTN_X];

/// Minus is Select on the wire: with A it opens the dial.
pub const MODIFIER: u32 = gamepad::BTN_BACK;

/// The escape chord, punktfunk's L1+R1+Start+Select: L+R+Plus+Minus.
const CHORD: u32 = gamepad::BTN_LB | gamepad::BTN_RB | gamepad::BTN_START | MODIFIER;

/// How long [`CHORD`] is held before it ends the stream. Its buttons are game input too.
pub const CHORD_HOLD: Duration = Duration::from_millis(1000);

/// Every button bit a Switch pad reports, for releases. SL and SR ride the paddles.
const BUTTONS: [u32; 20] = [
    gamepad::BTN_A,
    gamepad::BTN_B,
    gamepad::BTN_X,
    gamepad::BTN_Y,
    gamepad::BTN_BACK,
    gamepad::BTN_GUIDE,
    gamepad::BTN_START,
    gamepad::BTN_LS_CLICK,
    gamepad::BTN_RS_CLICK,
    gamepad::BTN_LB,
    gamepad::BTN_RB,
    gamepad::BTN_DPAD_UP,
    gamepad::BTN_DPAD_DOWN,
    gamepad::BTN_DPAD_LEFT,
    gamepad::BTN_DPAD_RIGHT,
    gamepad::BTN_MISC1,
    gamepad::BTN_PADDLE1,
    gamepad::BTN_PADDLE2,
    gamepad::BTN_PADDLE3,
    gamepad::BTN_PADDLE4,
];

/// What one step produced besides wire input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Routed {
    /// Minus+A: open the dial (or close it).
    ToggleDial,
    /// A press the open dial takes.
    Dial(MenuEvent),
    /// The escape chord was held for [`CHORD_HOLD`].
    Chord,
}

struct Slot {
    last: PadSample,
    dial: PadDial,
}

/// Per-slot state across samples.
#[derive(Default)]
pub struct PadRouter {
    slots: [Option<Slot>; SLOTS],
    chord_since: Option<Instant>,
}

impl PadRouter {
    /// Diffs one batch against the last. `dial_open` is whether the dial is up now; `kind` is
    /// the pad kind the session asked for (`Auto` mirrors the pad). Wire events land in `wire`.
    pub fn step(
        &mut self,
        batch: &[PadSample],
        now: Instant,
        dial_open: bool,
        kind: GamepadType,
        wire: &mut Vec<InputEvent>,
        routed: &mut Vec<Routed>,
    ) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            let pad = index as u8;
            let Some(sample) = batch.iter().find(|s| usize::from(s.slot) == index) else {
                if let Some(gone) = slot.take() {
                    release_into(pad, &gone.last, wire);
                    wire.push(remove_event(pad));
                }
                continue;
            };
            let state = slot.get_or_insert_with(|| {
                wire.extend(arrival_event(resolve_kind(kind, sample.kind), pad));
                Slot {
                    last: PadSample {
                        slot: sample.slot,
                        kind: sample.kind,
                        ..PadSample::default()
                    },
                    dial: PadDial::with_face(FACE).with_modifier(MODIFIER),
                }
            });
            diff(pad, state, sample, dial_open, wire, routed);
        }
        let held = self.slots.iter().flatten().any(|s| s.last.buttons & CHORD == CHORD);
        self.chord_since = match (held, self.chord_since) {
            (true, Some(t)) if now.duration_since(t) >= CHORD_HOLD => {
                routed.push(Routed::Chord);
                None
            }
            (true, since) => Some(since.unwrap_or(now)),
            (false, _) => None,
        };
    }

    /// The dial opened: release what the host still holds from every pad.
    pub fn dial_opened(&mut self, wire: &mut Vec<InputEvent>) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if let Some(slot) = slot {
                let sent = slot.dial.opened();
                for bit in BUTTONS.into_iter().filter(|b| sent & b != 0) {
                    wire.push(bit_event(bit, false, index as u8));
                }
            }
        }
    }

    pub fn dial_closed(&mut self) {
        for slot in self.slots.iter_mut().flatten() {
            slot.dial.closed();
        }
    }

    /// Unplugs every pad from the host and plugs it in again as `kind`: the dial's Controller
    /// type, for this stream only. The slots stay, so a button held through it is not pressed
    /// again: the A that picked the type would pick the next one, a frame later, until let go.
    pub fn replug_all(&mut self, kind: GamepadType, wire: &mut Vec<InputEvent>) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if let Some(slot) = slot {
                let pad = index as u8;
                release_into(pad, &slot.last, wire);
                wire.push(remove_event(pad));
                wire.extend(arrival_event(resolve_kind(kind, slot.last.kind), pad));
            }
        }
    }

    /// Lets go of everything the host holds and forgets the pads, so the next step re-declares
    /// them. For focus loss and every way out of a stream.
    pub fn release_all(&mut self, wire: &mut Vec<InputEvent>) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if let Some(gone) = slot.take() {
                release_into(index as u8, &gone.last, wire);
            }
        }
        self.chord_since = None;
    }
}

/// The kind the host builds for `pad`: the setting, or on Automatic the pad's own kind. A lone
/// Joy-Con has no kind of its own on the wire; the Pro Controller is the nearest whole pad.
pub(crate) fn resolve_kind(setting: GamepadType, pad: PadKind) -> GamepadType {
    match (setting, pad) {
        (GamepadType::Auto, PadKind::JoyConPair) => GamepadType::JoyConPair,
        (GamepadType::Auto, PadKind::SwitchPro | PadKind::JoyConLeft | PadKind::JoyConRight) => GamepadType::SwitchPro,
        (setting, _) => setting,
    }
}

fn diff(
    pad: u8,
    state: &mut Slot,
    now: &PadSample,
    dial_open: bool,
    wire: &mut Vec<InputEvent>,
    routed: &mut Vec<Routed>,
) {
    let changed = state.last.buttons ^ now.buttons;
    for bit in BUTTONS.into_iter().filter(|b| changed & b != 0) {
        let down = now.buttons & bit != 0;
        match state.dial.button(bit, down, dial_open) {
            PadRoute::Forward => wire.push(bit_event(bit, down, pad)),
            PadRoute::Drop => {}
            PadRoute::Open | PadRoute::Menu(MenuEvent::Back) => routed.push(Routed::ToggleDial),
            PadRoute::Menu(ev) => routed.push(Routed::Dial(ev)),
        }
    }
    let axes = [
        (gamepad::AXIS_LS_X, state.last.lx, now.lx),
        (gamepad::AXIS_LS_Y, state.last.ly, now.ly),
        (gamepad::AXIS_RS_X, state.last.rx, now.rx),
        (gamepad::AXIS_RS_Y, state.last.ry, now.ry),
    ];
    for (axis, was, is) in axes {
        if was == is {
            continue;
        }
        let left = axis == gamepad::AXIS_LS_X || axis == gamepad::AXIS_LS_Y;
        if left && dial_open {
            // The dial aims with the menu convention, +y down.
            let value = if axis == gamepad::AXIS_LS_Y {
                is.saturating_neg()
            } else {
                is
            };
            if let Some(ev) = state.dial.left_stick(axis == gamepad::AXIS_LS_X, value, true) {
                routed.push(Routed::Dial(ev));
            }
            continue;
        }
        wire.push(axis_event(axis, i32::from(is), pad));
    }
    for (axis, was, is) in [
        (gamepad::AXIS_LT, state.last.lt, now.lt),
        (gamepad::AXIS_RT, state.last.rt, now.rt),
    ] {
        if was != is {
            wire.push(axis_event(axis, i32::from(is), pad));
        }
    }
    state.last = *now;
}

/// Releases for everything `last` held: button ups and centred axes.
fn release_into(pad: u8, last: &PadSample, wire: &mut Vec<InputEvent>) {
    for bit in BUTTONS.into_iter().filter(|b| last.buttons & b != 0) {
        wire.push(bit_event(bit, false, pad));
    }
    let axes = [
        (gamepad::AXIS_LS_X, i32::from(last.lx)),
        (gamepad::AXIS_LS_Y, i32::from(last.ly)),
        (gamepad::AXIS_RS_X, i32::from(last.rx)),
        (gamepad::AXIS_RS_Y, i32::from(last.ry)),
        (gamepad::AXIS_LT, i32::from(last.lt)),
        (gamepad::AXIS_RT, i32::from(last.rt)),
    ];
    for (axis, value) in axes {
        if value != 0 {
            wire.push(axis_event(axis, 0, pad));
        }
    }
}

/// The pads folded into the one sample the shared menu synthesizer steps: buttons OR'd by
/// label ([`FACE`]), the stick from whichever is furthest off centre.
pub fn menu_sample(batch: &[PadSample]) -> MenuSample {
    let mut out = MenuSample::default();
    let mut best = -1i64;
    for s in batch {
        let b = |bit: u32| s.buttons & bit != 0;
        let [a, back, x, y] = FACE;
        for (o, v) in out
            .buttons
            .iter_mut()
            .zip([b(a), b(back), b(x), b(y), b(gamepad::BTN_LB), b(gamepad::BTN_RB)])
        {
            *o |= v;
        }
        for (o, v) in out.dpad.iter_mut().zip([
            b(gamepad::BTN_DPAD_UP),
            b(gamepad::BTN_DPAD_DOWN),
            b(gamepad::BTN_DPAD_LEFT),
            b(gamepad::BTN_DPAD_RIGHT),
        ]) {
            *o |= v;
        }
        let mag = i64::from(s.lx).pow(2) + i64::from(s.ly).pow(2);
        if mag > best {
            best = mag;
            out.lx = s.lx;
            // The synthesizer reads +y as down; the wire's +y is up.
            out.ly = s.ly.saturating_neg();
        }
    }
    out
}

pub fn bit_event(bit: u32, pressed: bool, pad: u8) -> InputEvent {
    InputEvent {
        kind: InputKind::GamepadButton,
        _pad: [0; 3],
        code: bit,
        x: i32::from(pressed),
        y: 0,
        flags: u32::from(pad),
    }
}

/// Sticks in the wire's range and sign (+y up), triggers 0..=255.
pub fn axis_event(axis: u32, value: i32, pad: u8) -> InputEvent {
    InputEvent {
        kind: InputKind::GamepadAxis,
        _pad: [0; 3],
        code: axis,
        x: value,
        y: 0,
        flags: u32::from(pad),
    }
}

/// Declares pad `pad`'s kind to the host. `None` for `Auto`: the host's own choice is what it means.
pub fn arrival_event(kind: GamepadType, pad: u8) -> Option<InputEvent> {
    let pref = kind.to_core();
    if pref == punktfunk_core::config::GamepadPref::Auto {
        return None;
    }
    Some(InputEvent {
        kind: InputKind::GamepadArrival,
        _pad: [0; 3],
        code: u32::from(pref.to_u8()),
        x: 0,
        y: 0,
        flags: punktfunk_core::input::encode_gamepad_arrival(pad, 0),
    })
}

/// Pad `pad` is gone: the host tears its virtual device down.
pub fn remove_event(pad: u8) -> InputEvent {
    InputEvent {
        kind: InputKind::GamepadRemove,
        _pad: [0; 3],
        code: 0,
        x: 0,
        y: 0,
        flags: punktfunk_core::input::encode_gamepad_remove(pad, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad(buttons: u32) -> PadSample {
        PadSample {
            slot: 0,
            kind: PadKind::JoyConPair,
            buttons,
            ..PadSample::default()
        }
    }

    fn step(router: &mut PadRouter, batch: &[PadSample], at: Instant, open: bool) -> (Vec<InputEvent>, Vec<Routed>) {
        let (mut wire, mut routed) = (Vec::new(), Vec::new());
        router.step(batch, at, open, GamepadType::Auto, &mut wire, &mut routed);
        (wire, routed)
    }

    #[test]
    fn a_new_pad_arrives_and_presses_go_out_once() {
        let mut r = PadRouter::default();
        let t = Instant::now();
        let (wire, _) = step(&mut r, &[pad(0)], t, false);
        assert_eq!(wire.len(), 1);
        assert_eq!(wire[0].kind, InputKind::GamepadArrival);
        let (wire, _) = step(&mut r, &[pad(gamepad::BTN_A)], t, false);
        assert_eq!(wire, [bit_event(gamepad::BTN_A, true, 0)]);
        let (wire, _) = step(&mut r, &[pad(gamepad::BTN_A)], t, false);
        assert!(wire.is_empty(), "a held button is not re-sent");
    }

    #[test]
    fn a_vanished_pad_releases_then_leaves() {
        let mut r = PadRouter::default();
        let t = Instant::now();
        step(&mut r, &[pad(gamepad::BTN_A)], t, false);
        let (wire, _) = step(&mut r, &[], t, false);
        assert_eq!(wire, [bit_event(gamepad::BTN_A, false, 0), remove_event(0)]);
    }

    #[test]
    fn minus_and_a_open_the_dial_without_reaching_the_host() {
        let mut r = PadRouter::default();
        let t = Instant::now();
        step(&mut r, &[pad(0)], t, false);
        let (wire, _) = step(&mut r, &[pad(MODIFIER)], t, false);
        assert_eq!(wire, [bit_event(MODIFIER, true, 0)]);
        let (wire, routed) = step(&mut r, &[pad(MODIFIER | FACE[0])], t, false);
        assert!(wire.is_empty());
        assert_eq!(routed, [Routed::ToggleDial]);
    }

    #[test]
    fn the_chord_fires_once_after_its_hold() {
        let mut r = PadRouter::default();
        let t = Instant::now();
        step(&mut r, &[pad(CHORD)], t, false);
        let (_, routed) = step(&mut r, &[pad(CHORD)], t + Duration::from_millis(500), false);
        assert!(routed.is_empty());
        let (_, routed) = step(&mut r, &[pad(CHORD)], t + CHORD_HOLD, false);
        assert_eq!(routed, [Routed::Chord]);
        let (_, routed) = step(&mut r, &[pad(CHORD)], t + CHORD_HOLD * 2, false);
        assert!(routed.is_empty(), "re-arms only after a release");
    }

    #[test]
    fn replug_plugs_in_as_the_new_kind_and_a_held_button_stays_held() {
        let mut r = PadRouter::default();
        let (mut wire, mut routed) = (Vec::new(), Vec::new());
        let now = Instant::now();
        r.step(
            &[pad(gamepad::BTN_A)],
            now,
            false,
            GamepadType::Auto,
            &mut wire,
            &mut routed,
        );
        wire.clear();
        r.replug_all(GamepadType::DualSense, &mut wire);
        let kinds: Vec<InputKind> = wire.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            [
                InputKind::GamepadButton,
                InputKind::GamepadRemove,
                InputKind::GamepadArrival
            ]
        );
        assert_eq!(wire[2].code, u32::from(GamepadType::DualSense.to_core().to_u8()));
        wire.clear();
        r.step(
            &[pad(gamepad::BTN_A)],
            now,
            false,
            GamepadType::DualSense,
            &mut wire,
            &mut routed,
        );
        assert!(
            wire.is_empty() && routed.is_empty(),
            "the held button is not a new press"
        );
    }

    #[test]
    fn release_all_centres_sticks_and_lifts_buttons() {
        let mut r = PadRouter::default();
        let t = Instant::now();
        let held = PadSample {
            lx: 1000,
            rt: 200,
            ..pad(gamepad::BTN_B)
        };
        step(&mut r, &[held], t, false);
        let mut wire = Vec::new();
        r.release_all(&mut wire);
        assert!(wire.contains(&bit_event(gamepad::BTN_B, false, 0)));
        assert!(wire.contains(&axis_event(gamepad::AXIS_LS_X, 0, 0)));
        assert!(wire.contains(&axis_event(gamepad::AXIS_RT, 0, 0)));
    }

    #[test]
    fn the_menu_sample_flips_y_and_ors_buttons() {
        let a = PadSample {
            ly: 20_000,
            ..pad(gamepad::BTN_B)
        };
        let b = PadSample {
            slot: 1,
            ..pad(gamepad::BTN_DPAD_LEFT)
        };
        let s = menu_sample(&[a, b]);
        assert!(s.buttons[0] && s.dpad[2]);
        assert_eq!(s.ly, -20_000);
    }

    /// A (east, the wire's `BTN_B`) confirms and B (south) backs in the console.
    #[test]
    fn east_confirms_and_south_backs() {
        use pf_client_core::menu_nav::MenuNav;
        let mut nav = MenuNav::new();
        let t = Instant::now();
        let mut out = Vec::new();
        nav.poll(&menu_sample(&[pad(0)]), t, &mut out);
        nav.poll(&menu_sample(&[pad(gamepad::BTN_B)]), t, &mut out);
        nav.poll(&menu_sample(&[pad(0)]), t, &mut out);
        nav.poll(&menu_sample(&[pad(gamepad::BTN_A)]), t, &mut out);
        assert_eq!(out, [MenuEvent::Confirm, MenuEvent::Back]);
    }

    #[test]
    fn automatic_declares_the_pad_in_hand() {
        assert_eq!(
            resolve_kind(GamepadType::Auto, PadKind::JoyConPair),
            GamepadType::JoyConPair
        );
        assert_eq!(
            resolve_kind(GamepadType::Auto, PadKind::JoyConLeft),
            GamepadType::SwitchPro
        );
        assert_eq!(
            resolve_kind(GamepadType::XboxOne, PadKind::SwitchPro),
            GamepadType::XboxOne
        );
    }
}
