//! hid samples into the wire's vocabulary. Pure, so it tests anywhere.
//!
//! Face buttons go by position, as every client sends them: the wire's `BTN_A` is the south
//! button, which a Nintendo pad labels B. Sticks keep libnx's sign (+y up) in the wire's i16
//! range; ZL and ZR are digital, so they press their trigger fully.

use punktfunk_core::input::gamepad as wire;

/// libnx `HidNpadButton` bits (`switch/services/hid.h`).
pub mod npad {
    pub const A: u64 = 1 << 0;
    pub const B: u64 = 1 << 1;
    pub const X: u64 = 1 << 2;
    pub const Y: u64 = 1 << 3;
    pub const STICK_L: u64 = 1 << 4;
    pub const STICK_R: u64 = 1 << 5;
    pub const L: u64 = 1 << 6;
    pub const R: u64 = 1 << 7;
    pub const ZL: u64 = 1 << 8;
    pub const ZR: u64 = 1 << 9;
    pub const PLUS: u64 = 1 << 10;
    pub const MINUS: u64 = 1 << 11;
    pub const LEFT: u64 = 1 << 12;
    pub const UP: u64 = 1 << 13;
    pub const RIGHT: u64 = 1 << 14;
    pub const DOWN: u64 = 1 << 15;
    pub const LEFT_SL: u64 = 1 << 24;
    pub const LEFT_SR: u64 = 1 << 25;
    pub const RIGHT_SL: u64 = 1 << 26;
    pub const RIGHT_SR: u64 = 1 << 27;
}

// TODO(verify): a single Joy-Con held sideways: whether hid already turns its face buttons and
// stick to the hold, and whether SL/SR as shoulders feel right (probe 8 lists what it reports).
const BUTTONS: [(u64, u32); 16] = [
    (npad::B, wire::BTN_A),
    (npad::A, wire::BTN_B),
    (npad::Y, wire::BTN_X),
    (npad::X, wire::BTN_Y),
    (npad::L, wire::BTN_LB),
    (npad::R, wire::BTN_RB),
    (npad::LEFT_SL | npad::RIGHT_SL, wire::BTN_LB),
    (npad::LEFT_SR | npad::RIGHT_SR, wire::BTN_RB),
    (npad::STICK_L, wire::BTN_LS_CLICK),
    (npad::STICK_R, wire::BTN_RS_CLICK),
    (npad::PLUS, wire::BTN_START),
    (npad::MINUS, wire::BTN_BACK),
    (npad::UP, wire::BTN_DPAD_UP),
    (npad::DOWN, wire::BTN_DPAD_DOWN),
    (npad::LEFT, wire::BTN_DPAD_LEFT),
    (npad::RIGHT, wire::BTN_DPAD_RIGHT),
];

/// `HidNpadButton` bits to the wire's `BTN_*` bits.
pub fn buttons(npad: u64) -> u32 {
    BUTTONS
        .iter()
        .filter(|(n, _)| npad & n != 0)
        .fold(0, |acc, (_, w)| acc | w)
}

/// ZL and ZR as the wire's triggers, 0 or 255.
pub fn triggers(npad: u64) -> (u8, u8) {
    let full = |bit: u64| if npad & bit != 0 { u8::MAX } else { 0 };
    (full(npad::ZL), full(npad::ZR))
}

/// One libnx stick axis to the wire's range.
pub fn stick(raw: i32) -> i16 {
    raw.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

/// The HD rumble pair the core's motor levels drive: the low motor on 160 Hz, the high one on
/// 320 Hz.
pub const RUMBLE_HZ: (f32, f32) = (160.0, 320.0);

/// Motor levels 0..=65535 to HD rumble amplitudes 0.0..=1.0, low then high.
pub fn rumble(low: u16, high: u16) -> (f32, f32) {
    let amp = |level: u16| f32::from(level) / f32::from(u16::MAX);
    (amp(low), amp(high))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn face_buttons_go_by_position() {
        assert_eq!(buttons(npad::A), wire::BTN_B, "A is east");
        assert_eq!(buttons(npad::B), wire::BTN_A, "B is south");
        assert_eq!(buttons(npad::X), wire::BTN_Y, "X is north");
        assert_eq!(buttons(npad::Y), wire::BTN_X, "Y is west");
    }

    #[test]
    fn the_rest_map_by_name() {
        let held = npad::PLUS | npad::MINUS | npad::L | npad::R | npad::STICK_L | npad::UP;
        assert_eq!(
            buttons(held),
            wire::BTN_START | wire::BTN_BACK | wire::BTN_LB | wire::BTN_RB | wire::BTN_LS_CLICK | wire::BTN_DPAD_UP
        );
        assert_eq!(buttons(npad::ZL | npad::ZR), 0, "triggers are axes");
        assert_eq!(triggers(npad::ZR), (0, 255));
    }

    #[test]
    fn sticks_and_rumble_stay_in_range() {
        assert_eq!(stick(32767), 32767);
        assert_eq!(stick(-40000), -32768);
        assert_eq!(stick(-1200), -1200);
        assert_eq!(rumble(0, u16::MAX), (0.0, 1.0));
    }
}
