//! Pad samples into what the loops act on: wire input for the host, menu events or input test
//! readings for the console, the dial's own presses, and the shortcuts. Pure: samples in, events
//! out.

use std::time::Instant;

use pf_client_core::menu_nav::{MenuEvent, MenuNav, MenuSample};
use pf_client_core::overlay_actions::{OverlayConfig, SlotId};
use pf_client_core::ring::RingCommand;
use pf_client_core::shortcuts::{Action, Binding, Device, Engine, Event, Out, Phase, Shortcuts};
use pf_console_ui::PadTestState;
use punktfunk_core::input::{gamepad, key_vk, InputEvent, InputKind};

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
#[derive(Clone, Debug, PartialEq)]
pub enum Routed {
    /// A shortcut opened the dial or closed it, or B closed it.
    ToggleDial,
    /// A press the open dial takes.
    Dial(MenuEvent),
    /// A shortcut fired this quick action.
    Command(RingCommand),
}

/// Key combinations by id, their keys as VKs.
type Keys = [(String, Vec<u8>)];

struct Slot {
    last: PadSample,
    dial: PadDial,
    engine: Engine,
}

/// Per-slot state across samples, and the shortcuts every slot's engine runs.
pub struct PadRouter {
    slots: [Option<Slot>; SLOTS],
    bindings: Vec<Binding>,
    keys: Vec<(String, Vec<u8>)>,
}

impl PadRouter {
    /// Runs the bindings this console acts on: the dial, the quick actions the stream runs
    /// without it, and the key combinations `ring` names. The rest stay game input.
    pub fn new(shortcuts: &Shortcuts, ring: &OverlayConfig) -> Self {
        let keys: Vec<(String, Vec<u8>)> = (ring.shortcuts.iter())
            .filter_map(|s| Some((s.id.clone(), s.keys.iter().map(|k| key_vk(k)).collect::<Option<_>>()?)))
            .collect();
        let bindings = (shortcuts.bindings.iter())
            .filter(|b| match &b.action {
                Action::Ring => true,
                Action::Slot(SlotId::Shortcut(id)) => keys.iter().any(|(k, _)| k == id),
                Action::Slot(slot) => matches!(
                    slot,
                    SlotId::EndStream | SlotId::DisconnectLinger | SlotId::Stats | SlotId::PadMouse
                ),
                Action::Unknown(_) => false,
            })
            .cloned()
            .collect();
        Self {
            slots: Default::default(),
            bindings,
            keys,
        }
    }

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
                if let Some(mut gone) = slot.take() {
                    gone.let_go(pad, &self.keys, wire);
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
                    dial: PadDial::with_face(FACE),
                    engine: Engine::new(&self.bindings, Device::Pad),
                }
            });
            let due = state.engine.tick(now);
            state.apply(pad, due, dial_open, &self.keys, wire, routed);
            state.diff(pad, sample, now, dial_open, &self.keys, wire, routed);
        }
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
            slot.dial.closed(slot.last.buttons);
        }
    }

    /// Unplugs every pad from the host and plugs it in again as `kind`: the dial's Controller
    /// type, for this stream only. The slots stay, so a button held through it is not pressed
    /// again: the A that picked the type would pick the next one, a frame later, until let go.
    pub fn replug_all(&mut self, kind: GamepadType, wire: &mut Vec<InputEvent>) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if let Some(slot) = slot {
                let pad = index as u8;
                release_into(pad, slot.dial.take_sent(), &slot.last, wire);
                wire.push(remove_event(pad));
                wire.extend(arrival_event(resolve_kind(kind, slot.last.kind), pad));
            }
        }
    }

    /// Lets go of everything the host holds and forgets the pads, so the next step re-declares
    /// them. For focus loss and every way out of a stream.
    pub fn release_all(&mut self, wire: &mut Vec<InputEvent>) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if let Some(mut gone) = slot.take() {
                gone.let_go(index as u8, &self.keys, wire);
            }
        }
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

impl Slot {
    /// One sample: button edges through the engine, axes straight on, the left stick aiming
    /// the open dial instead.
    #[allow(clippy::too_many_arguments)]
    fn diff(
        &mut self,
        pad: u8,
        sample: &PadSample,
        now: Instant,
        dial_open: bool,
        keys: &Keys,
        wire: &mut Vec<InputEvent>,
        routed: &mut Vec<Routed>,
    ) {
        let changed = self.last.buttons ^ sample.buttons;
        for bit in BUTTONS.into_iter().filter(|b| changed & b != 0) {
            let ev = if sample.buttons & bit != 0 {
                Event::Press(bit)
            } else {
                Event::Release(bit)
            };
            let out = self.engine.step(ev, now);
            self.apply(pad, out, dial_open, keys, wire, routed);
        }
        let axes = [
            (gamepad::AXIS_LS_X, self.last.lx, sample.lx),
            (gamepad::AXIS_LS_Y, self.last.ly, sample.ly),
            (gamepad::AXIS_RS_X, self.last.rx, sample.rx),
            (gamepad::AXIS_RS_Y, self.last.ry, sample.ry),
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
                if let Some(ev) = self.dial.left_stick(axis == gamepad::AXIS_LS_X, value, true) {
                    routed.push(Routed::Dial(ev));
                }
                continue;
            }
            wire.push(axis_event(axis, i32::from(is), pad));
        }
        for (axis, was, is) in [
            (gamepad::AXIS_LT, self.last.lt, sample.lt),
            (gamepad::AXIS_RT, self.last.rt, sample.rt),
        ] {
            if was != is {
                wire.push(axis_event(axis, i32::from(is), pad));
            }
        }
        self.last = *sample;
    }

    /// The engine's outputs: a forwarded edge goes to the host or the open dial, a lift
    /// releases what the host holds, and a fired shortcut runs.
    fn apply(
        &mut self,
        pad: u8,
        out: Vec<Out>,
        dial_open: bool,
        keys: &Keys,
        wire: &mut Vec<InputEvent>,
        routed: &mut Vec<Routed>,
    ) {
        for o in out {
            match o {
                Out::Forward(bit, down) => match self.dial.button(bit, down, dial_open) {
                    PadRoute::Forward => wire.push(bit_event(bit, down, pad)),
                    PadRoute::Drop => {}
                    PadRoute::Menu(MenuEvent::Back) => routed.push(Routed::ToggleDial),
                    PadRoute::Menu(ev) => routed.push(Routed::Dial(ev)),
                },
                Out::Lift(bits) => {
                    for bit in bits.into_iter().filter(|&b| self.dial.lift(b)) {
                        wire.push(bit_event(bit, false, pad));
                    }
                }
                Out::Fire(action, phase) => fire(&action, phase, keys, wire, routed),
            }
        }
    }

    /// The pad is gone or the stream lets go: a held key combination comes up, and the host
    /// lets go of every button and axis this slot holds there.
    fn let_go(&mut self, pad: u8, keys: &Keys, wire: &mut Vec<InputEvent>) {
        let out = self.engine.flush();
        self.apply(pad, out, false, keys, wire, &mut Vec::new());
        release_into(pad, self.dial.take_sent(), &self.last, wire);
    }
}

/// A fired shortcut: the dial toggles, a key combination goes down or up on the host, a quick
/// action goes to the main thread.
fn fire(action: &Action, phase: Phase, keys: &Keys, wire: &mut Vec<InputEvent>, routed: &mut Vec<Routed>) {
    match action {
        Action::Ring => routed.push(Routed::ToggleDial),
        Action::Slot(SlotId::Shortcut(id)) => {
            let Some((_, vks)) = keys.iter().find(|(k, _)| k == id) else {
                return;
            };
            if matches!(phase, Phase::Down | Phase::Tap) {
                wire.extend(vks.iter().map(|&vk| key_event(InputKind::KeyDown, vk)));
            }
            if matches!(phase, Phase::Up | Phase::Tap) {
                wire.extend(vks.iter().rev().map(|&vk| key_event(InputKind::KeyUp, vk)));
            }
        }
        Action::Slot(slot) => routed.extend(RingCommand::for_slot(slot).map(Routed::Command)),
        Action::Unknown(_) => {}
    }
}

/// Releases for `sent`, the buttons the host holds, and centred axes for what `last` held.
fn release_into(pad: u8, sent: u32, last: &PadSample, wire: &mut Vec<InputEvent>) {
    for bit in BUTTONS.into_iter().filter(|b| sent & b != 0) {
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

/// The console's input test names for the wire bits. Its Nintendo legend shows `B` as A.
const TEST_NAMES: [(u32, &str); 20] = [
    (gamepad::BTN_A, "A"),
    (gamepad::BTN_B, "B"),
    (gamepad::BTN_X, "X"),
    (gamepad::BTN_Y, "Y"),
    (gamepad::BTN_LB, "LB"),
    (gamepad::BTN_RB, "RB"),
    (gamepad::BTN_BACK, "Back"),
    (gamepad::BTN_START, "Start"),
    (gamepad::BTN_GUIDE, "Guide"),
    (gamepad::BTN_LS_CLICK, "LS"),
    (gamepad::BTN_RS_CLICK, "RS"),
    (gamepad::BTN_DPAD_UP, "Up"),
    (gamepad::BTN_DPAD_DOWN, "Down"),
    (gamepad::BTN_DPAD_LEFT, "Left"),
    (gamepad::BTN_DPAD_RIGHT, "Right"),
    (gamepad::BTN_MISC1, "Misc"),
    (gamepad::BTN_PADDLE1, "R4"),
    (gamepad::BTN_PADDLE2, "L4"),
    (gamepad::BTN_PADDLE3, "R5"),
    (gamepad::BTN_PADDLE4, "L5"),
];

/// The pads as the console's input test reads them: buttons held on any pad except `hide`, a
/// trigger past half travel as held, and the axes of the pad pushed furthest (+y down).
pub fn pad_test_state(batch: &[PadSample], hide: u32) -> PadTestState {
    let buttons = batch.iter().fold(0, |b, s| b | s.buttons) & !hide;
    let mut held: Vec<String> = (TEST_NAMES.iter())
        .filter(|(bit, _)| buttons & bit != 0)
        .map(|(_, name)| (*name).to_string())
        .collect();
    for (name, down) in [
        ("LT", batch.iter().any(|s| s.lt > 127)),
        ("RT", batch.iter().any(|s| s.rt > 127)),
    ] {
        if down {
            held.push(name.to_string());
        }
    }
    let stick = |v: i16| (f32::from(v) / 32767.0).clamp(-1.0, 1.0);
    let trigger = |v: u8| f32::from(v) / 255.0;
    let reach = |a: &[f32; 6]| a.iter().map(|v| v * v).sum::<f32>();
    let axes = (batch.iter())
        .map(|s| {
            [
                stick(s.lx),
                -stick(s.ly),
                stick(s.rx),
                -stick(s.ry),
                trigger(s.lt),
                trigger(s.rt),
            ]
        })
        .max_by(|a, b| reach(a).total_cmp(&reach(b)))
        .unwrap_or_default();
    let axes = (["LX", "LY", "RX", "RY", "LT", "RT"].iter())
        .zip(axes)
        .map(|(name, v)| ((*name).to_string(), v))
        .collect();
    PadTestState { held, axes }
}

/// The console's pads: menu events, or while its input test runs, readings for that instead.
#[derive(Default)]
pub struct MenuPads {
    nav: MenuNav,
    /// The last reading sent; `None` while the test is off.
    test_last: Option<PadTestState>,
    /// Held as the test began, kept out of it until let go. The A that opened it is the east
    /// button, which the console's own check for that press (a south A) misses.
    test_hide: u32,
}

impl MenuPads {
    /// One poll. While `testing` nothing moves the menu and a changed reading comes back; what is
    /// still held as the test ends counts as down, not pressed.
    pub fn poll(
        &mut self,
        batch: &[PadSample],
        testing: bool,
        now: Instant,
        out: &mut Vec<MenuEvent>,
    ) -> Option<PadTestState> {
        if !testing {
            self.test_last = None;
            self.nav.poll(&menu_sample(batch), now, out);
            return None;
        }
        self.nav.reset();
        let held = batch.iter().fold(0, |b, s| b | s.buttons);
        if self.test_last.is_none() {
            self.test_hide = held;
        }
        self.test_hide &= held;
        let state = pad_test_state(batch, self.test_hide);
        if self.test_last.as_ref() == Some(&state) {
            return None;
        }
        self.test_last = Some(state.clone());
        Some(state)
    }
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

/// A keyboard key by VK, down or up as `kind` says.
pub fn key_event(kind: InputKind, vk: u8) -> InputEvent {
    InputEvent {
        kind,
        _pad: [0; 3],
        code: u32::from(vk),
        x: 0,
        y: 0,
        flags: 0,
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
    use std::time::Duration;

    use pf_client_core::overlay_actions::{RingPlatform, Shortcut};
    use pf_client_core::shortcuts::{defaults, Client, Kind, Trigger, HOLD, TAP_PRESS};

    use super::*;

    /// L+R+Plus+Minus.
    const EXIT: u32 = gamepad::BTN_LB | gamepad::BTN_RB | gamepad::BTN_START | MODIFIER;

    fn router() -> PadRouter {
        PadRouter::new(
            &defaults(Client::Switch, false, ""),
            &OverlayConfig::platform_default(RingPlatform::Desktop),
        )
    }

    /// The Shortcuts page names the defaults by the Switch's own buttons.
    #[test]
    fn the_page_lists_the_defaults_in_nintendo_names() {
        let groups = defaults(Client::Switch, false, "").rows(Client::Switch, false);
        let pad = groups.iter().find(|g| g.kind == Kind::Pad).expect("a pad group");
        let rows: Vec<String> = pad.items.iter().map(|i| i.keys.to_string()).collect();
        assert_eq!(rows, ["Minus + A", "Hold L + R + Plus + Minus"]);
    }

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
        let mut r = router();
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
        let mut r = router();
        let t = Instant::now();
        step(&mut r, &[pad(gamepad::BTN_A)], t, false);
        let (wire, _) = step(&mut r, &[], t, false);
        assert_eq!(wire, [bit_event(gamepad::BTN_A, false, 0), remove_event(0)]);
    }

    #[test]
    fn minus_and_a_open_the_dial_without_reaching_the_host() {
        let mut r = router();
        let t = Instant::now();
        step(&mut r, &[pad(0)], t, false);
        let (wire, _) = step(&mut r, &[pad(MODIFIER)], t, false);
        assert_eq!(wire, [bit_event(MODIFIER, true, 0)]);
        let (wire, routed) = step(&mut r, &[pad(MODIFIER | FACE[0])], t, false);
        assert_eq!(wire, [bit_event(MODIFIER, false, 0)], "the host lets go of Minus");
        assert_eq!(routed, [Routed::ToggleDial]);
        let (wire, routed) = step(&mut r, &[pad(0)], t, false);
        assert!(wire.is_empty() && routed.is_empty(), "the chord's releases stay home");
    }

    #[test]
    fn the_dial_closes_on_its_chord_and_minus_stays_the_games() {
        let mut r = router();
        let t = Instant::now();
        step(&mut r, &[pad(0)], t, true);
        let (wire, _) = step(&mut r, &[pad(MODIFIER)], t, true);
        assert!(wire.is_empty(), "the open dial owns every press");
        let (wire, routed) = step(&mut r, &[pad(MODIFIER | FACE[0])], t, true);
        assert!(wire.is_empty());
        assert_eq!(routed, [Routed::ToggleDial]);
        r.dial_closed();
        step(&mut r, &[pad(0)], t, false);
        let (wire, _) = step(&mut r, &[pad(MODIFIER)], t, false);
        assert_eq!(wire, [bit_event(MODIFIER, true, 0)]);
        let (wire, _) = step(&mut r, &[pad(0)], t, false);
        assert_eq!(wire, [bit_event(MODIFIER, false, 0)], "a fresh Minus lets go too");
    }

    #[test]
    fn the_escape_chord_ends_the_stream_after_its_hold() {
        let mut r = router();
        let t = Instant::now();
        step(&mut r, &[pad(0)], t, false);
        let (wire, _) = step(&mut r, &[pad(EXIT)], t, false);
        let pressed = [MODIFIER, gamepad::BTN_START, gamepad::BTN_LB];
        assert_eq!(wire, pressed.map(|b| bit_event(b, true, 0)), "R completes it and waits");
        let (_, routed) = step(&mut r, &[pad(EXIT)], t + Duration::from_millis(500), false);
        assert!(routed.is_empty());
        let (wire, routed) = step(&mut r, &[pad(EXIT)], t + HOLD, false);
        assert_eq!(routed, [Routed::Command(RingCommand::EndStream)]);
        assert_eq!(wire, pressed.map(|b| bit_event(b, false, 0)));
        let (_, routed) = step(&mut r, &[pad(EXIT)], t + HOLD * 2, false);
        assert!(routed.is_empty(), "re-arms only after a release");
    }

    #[test]
    fn a_short_escape_chord_is_game_input() {
        let mut r = router();
        let t = Instant::now();
        step(&mut r, &[pad(0)], t, false);
        step(&mut r, &[pad(EXIT)], t, false);
        let early = t + Duration::from_millis(200);
        let (wire, routed) = step(&mut r, &[pad(EXIT & !gamepad::BTN_RB)], early, false);
        assert_eq!(wire, [bit_event(gamepad::BTN_RB, true, 0)]);
        assert!(routed.is_empty());
        let (wire, _) = step(&mut r, &[pad(EXIT & !gamepad::BTN_RB)], early + TAP_PRESS, false);
        assert_eq!(wire, [bit_event(gamepad::BTN_RB, false, 0)], "a tap, its halves apart");
        let (_, routed) = step(&mut r, &[pad(EXIT & !gamepad::BTN_RB)], t + HOLD * 2, false);
        assert!(routed.is_empty());
    }

    #[test]
    fn a_held_key_combination_stays_down_until_its_button_lifts() {
        let mut ring = OverlayConfig::platform_default(RingPlatform::Desktop);
        ring.shortcuts.push(Shortcut {
            id: "switcher".into(),
            label: String::new(),
            keys: vec!["alt".into(), "tab".into()],
        });
        let hold = |bit, action| Binding {
            trigger: Trigger::Pad {
                buttons: vec![bit],
                hold: true,
            },
            action,
        };
        let bindings = vec![
            hold(MODIFIER, Action::Slot(SlotId::Shortcut("switcher".into()))),
            // No Guide on this console: X stays the game's.
            hold(gamepad::BTN_X, Action::Slot(SlotId::Guide)),
        ];
        let mut r = PadRouter::new(&Shortcuts { bindings }, &ring);
        let t = Instant::now();
        step(&mut r, &[pad(0)], t, false);
        let (wire, _) = step(&mut r, &[pad(gamepad::BTN_X)], t, false);
        assert_eq!(wire, [bit_event(gamepad::BTN_X, true, 0)]);
        step(&mut r, &[pad(0)], t, false);
        let (wire, _) = step(&mut r, &[pad(MODIFIER)], t, false);
        assert!(wire.is_empty(), "held back while the hold is armed");
        let [alt, tab] = ["alt", "tab"].map(|k| key_vk(k).expect("a known key"));
        let (wire, _) = step(&mut r, &[pad(MODIFIER)], t + HOLD, false);
        assert_eq!(
            wire,
            [key_event(InputKind::KeyDown, alt), key_event(InputKind::KeyDown, tab)]
        );
        let (wire, _) = step(&mut r, &[pad(0)], t + HOLD * 2, false);
        assert_eq!(
            wire,
            [key_event(InputKind::KeyUp, tab), key_event(InputKind::KeyUp, alt)]
        );
    }

    #[test]
    fn replug_plugs_in_as_the_new_kind_and_a_held_button_stays_held() {
        let mut r = router();
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
        let mut r = router();
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

    /// Wire names (east is `B`), a trigger past half travel held, +y down from the furthest pad.
    #[test]
    fn the_input_test_reads_wire_names_triggers_and_y_down() {
        let a = PadSample {
            ly: 32767,
            rt: 255,
            ..pad(gamepad::BTN_B | gamepad::BTN_PADDLE1)
        };
        let b = PadSample {
            slot: 1,
            lx: 100,
            lt: 100,
            ..pad(gamepad::BTN_DPAD_UP)
        };
        let s = pad_test_state(&[a, b], 0);
        assert_eq!(s.held, ["B", "Up", "R4", "RT"]);
        let axis = |n: &str| s.axes.iter().find(|(k, _)| k == n).map(|(_, v)| *v);
        assert_eq!(axis("LY"), Some(-1.0), "the wire's up is the test's -y");
        assert_eq!(axis("RT"), Some(1.0));
        assert_eq!(axis("LX"), Some(0.0), "from the pad pushed furthest");
        assert_eq!(pad_test_state(&[a], gamepad::BTN_B).held, ["R4", "RT"], "hidden");
    }

    #[test]
    fn the_input_test_takes_the_pads_off_the_menu() {
        let mut pads = MenuPads::default();
        let t = Instant::now();
        let mut out = Vec::new();
        let [a, b, ..] = FACE;
        pads.poll(&[pad(0)], false, t, &mut out);
        pads.poll(&[pad(a)], false, t, &mut out);
        assert_eq!(out, [MenuEvent::Confirm]);
        out.clear();
        let first = pads.poll(&[pad(a)], true, t, &mut out).expect("a first reading");
        assert!(first.held.is_empty(), "the A that opened the test stays out of it");
        assert!(
            pads.poll(&[pad(a)], true, t, &mut out).is_none(),
            "an unchanged reading"
        );
        pads.poll(&[pad(0)], true, t, &mut out);
        let lit = pads.poll(&[pad(b | gamepad::BTN_DPAD_DOWN)], true, t, &mut out);
        assert_eq!(lit.expect("B lights").held, ["A", "Down"]);
        assert!(out.is_empty(), "no menu moves while the test runs");
        pads.poll(&[pad(b)], false, t, &mut out);
        pads.poll(&[pad(0)], false, t, &mut out);
        assert!(out.is_empty(), "the B that left the test does not back out too");
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
