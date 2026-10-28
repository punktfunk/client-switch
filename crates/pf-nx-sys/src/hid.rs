//! Pads, HD rumble, motion and the touch screen. Externs from `runtime/pad.h` and
//! `services/hid.h`, libnx 4.12.0.
//!
//! Slot 0 reads player 1 and the handheld Joy-Cons together (libnx's `padInitializeDefault`
//! set); slot `n` reads player `n + 1`. A slot's rumble and motion go to the controller that is
//! connected there, handheld first.

/// `HidNpadIdType_Handheld`; `HidNpadIdType_No1..No8` are 0..7.
pub const NPAD_ID_HANDHELD: u32 = 0x20;
pub const STYLE_FULL_KEY: u32 = 1 << 0;
pub const STYLE_HANDHELD: u32 = 1 << 1;
pub const STYLE_JOY_DUAL: u32 = 1 << 2;
pub const STYLE_JOY_LEFT: u32 = 1 << 3;
pub const STYLE_JOY_RIGHT: u32 = 1 << 4;
/// `HidNpadStyleSet_NpadStandard`: full key, handheld, dual and single Joy-Cons.
pub const STYLE_SET_STANDARD: u32 = 0x1F;
pub const NPAD_ATTRIBUTE_IS_CONNECTED: u32 = 1 << 0;
pub const SIX_AXIS_ATTRIBUTE_IS_CONNECTED: u32 = 1 << 0;
/// `JOYSTICK_MAX`; a stick axis reads `-JOYSTICK_MAX..=JOYSTICK_MAX`.
pub const JOYSTICK_MAX: i32 = 0x7FFF;

/// `HidAnalogStickState`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct HidAnalogStickState {
    pub x: i32,
    pub y: i32,
}

/// `PadState` (runtime/pad.h). Its accessors are `static inline`, so the fields are read here.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PadState {
    pub id_mask: u8,
    pub active_id_mask: u8,
    pub read_handheld: bool,
    pub active_handheld: bool,
    pub style_set: u32,
    pub attributes: u32,
    pub buttons_cur: u64,
    pub buttons_old: u64,
    pub sticks: [HidAnalogStickState; 2],
    pub gc_triggers: [u32; 2],
}

/// `HidVibrationValue`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HidVibrationValue {
    pub amp_low: f32,
    pub freq_low: f32,
    pub amp_high: f32,
    pub freq_high: f32,
}

/// `HidVector`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct HidVector {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

/// `HidSixAxisSensorState`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct HidSixAxisSensorState {
    pub delta_time: u64,
    pub sampling_number: u64,
    pub acceleration: HidVector,
    pub angular_velocity: HidVector,
    pub angle: HidVector,
    /// `HidDirectionState`: a 3×3 matrix.
    pub direction: [[f32; 3]; 3],
    pub attributes: u32,
    pub reserved: u32,
}

/// `HidTouchState`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct HidTouchState {
    pub delta_time: u64,
    pub attributes: u32,
    pub finger_id: u32,
    pub x: u32,
    pub y: u32,
    pub diameter_x: u32,
    pub diameter_y: u32,
    pub rotation_angle: u32,
    pub reserved: u32,
}

/// `HidTouchScreenState`: `count` of the 16 `touches` are valid.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct HidTouchScreenState {
    pub sampling_number: u64,
    pub count: i32,
    pub reserved: u32,
    pub touches: [HidTouchState; 16],
}

/// The controller kind behind a slot, from its `HidNpadStyleTag` set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// Joy-Cons attached to the console.
    Handheld,
    JoyDual,
    JoyLeft,
    JoyRight,
    /// A Pro Controller.
    FullKey,
    Other,
}

impl Style {
    /// Handheld wins: slot 0 merges the handheld Joy-Cons with player 1.
    pub fn from_set(style_set: u32, handheld: bool) -> Self {
        if handheld || style_set & STYLE_HANDHELD != 0 {
            Self::Handheld
        } else if style_set & STYLE_FULL_KEY != 0 {
            Self::FullKey
        } else if style_set & STYLE_JOY_DUAL != 0 {
            Self::JoyDual
        } else if style_set & STYLE_JOY_LEFT != 0 {
            Self::JoyLeft
        } else if style_set & STYLE_JOY_RIGHT != 0 {
            Self::JoyRight
        } else {
            Self::Other
        }
    }
}

/// One slot's sample. Buttons are `HidNpadButton` bits; sticks are `±JOYSTICK_MAX`.
// TODO(verify): +y is up on the console (hid.h does not say).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PadRaw {
    pub connected: bool,
    pub style: Style,
    pub buttons: u64,
    pub lx: i32,
    pub ly: i32,
    pub rx: i32,
    pub ry: i32,
}

impl PadRaw {
    pub fn from_state(s: &PadState) -> Self {
        Self {
            connected: s.attributes & NPAD_ATTRIBUTE_IS_CONNECTED != 0,
            style: Style::from_set(s.style_set, s.active_handheld),
            buttons: s.buttons_cur,
            lx: s.sticks[0].x,
            ly: s.sticks[0].y,
            rx: s.sticks[1].x,
            ry: s.sticks[1].y,
        }
    }
}

/// One HD rumble actuator's value: amplitudes `0.0..=1.0`, frequencies in Hz.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vibration {
    pub amp_low: f32,
    pub freq_low: f32,
    pub amp_high: f32,
    pub freq_high: f32,
}

impl From<Vibration> for HidVibrationValue {
    /// Clamps the amplitudes to `0.0..=1.0` (NaN reads as 0); frequencies pass through.
    fn from(v: Vibration) -> Self {
        let amp = |a: f32| if a.is_nan() { 0.0 } else { a.clamp(0.0, 1.0) };
        Self {
            amp_low: amp(v.amp_low),
            freq_low: v.freq_low,
            amp_high: amp(v.amp_high),
            freq_high: v.freq_high,
        }
    }
}

/// One motion sample. hid.h names no units; `accel` is in G and `gyro` in rotations per second,
/// nn's convention. `timestamp` is the sensor's sampling number, a counter, not a clock.
// TODO(verify): the units on the console (WP0.8), and whether `delta_time` is nanoseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SixAxis {
    pub accel: [f32; 3],
    pub gyro: [f32; 3],
    pub timestamp: u64,
}

impl SixAxis {
    /// `None` when the sensor reports itself disconnected.
    pub fn from_state(s: &HidSixAxisSensorState) -> Option<Self> {
        if s.attributes & SIX_AXIS_ATTRIBUTE_IS_CONNECTED == 0 {
            return None;
        }
        let v = |h: HidVector| [h.x, h.y, h.z];
        Some(Self {
            accel: v(s.acceleration),
            gyro: v(s.angular_velocity),
            timestamp: s.sampling_number,
        })
    }
}

/// One finger on the touch screen.
// TODO(verify): coordinates are panel pixels, 1280×720, docked or not (hid.h does not say).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Touch {
    pub x: u32,
    pub y: u32,
    pub id: u32,
    pub diameter_x: u32,
    pub diameter_y: u32,
}

impl From<&HidTouchState> for Touch {
    fn from(t: &HidTouchState) -> Self {
        Self {
            x: t.x,
            y: t.y,
            id: t.finger_id,
            diameter_x: t.diameter_x,
            diameter_y: t.diameter_y,
        }
    }
}

/// The `(HidNpadIdType, HidNpadStyleTag)` a slot's rumble and motion address, or `None` when
/// nothing standard is connected there.
pub fn npad_target(slot: usize, s: &PadState) -> Option<(u32, u32)> {
    if s.active_handheld {
        return Some((NPAD_ID_HANDHELD, STYLE_HANDHELD));
    }
    if s.active_id_mask == 0 {
        return None;
    }
    let tag = [STYLE_FULL_KEY, STYLE_JOY_DUAL, STYLE_JOY_LEFT, STYLE_JOY_RIGHT]
        .into_iter()
        .find(|t| s.style_set & t != 0)?;
    Some((slot as u32, tag))
}

/// The values for a style's vibration handles, `[left, right]` in: a single Joy-Con has one.
pub fn vibration_values(style: u32, v: [Vibration; 2]) -> ([HidVibrationValue; 2], usize) {
    let (l, r) = (v[0].into(), v[1].into());
    match style {
        STYLE_JOY_LEFT => ([l, l], 1),
        STYLE_JOY_RIGHT => ([r, r], 1),
        _ => ([l, r], 2),
    }
}

#[cfg(target_os = "horizon")]
mod ffi {
    use super::{HidSixAxisSensorState, HidTouchScreenState, HidVibrationValue, PadState};

    // `HidVibrationDeviceHandle` and `HidSixAxisSensorHandle` are unions of one `u32`.
    extern "C" {
        pub fn padConfigureInput(max_players: u32, style_set: u32);
        pub fn padInitializeWithMask(pad: *mut PadState, mask: u64);
        pub fn padUpdate(pad: *mut PadState);
        pub fn hidInitializeVibrationDevices(handles: *mut u32, total_handles: i32, id: u32, style: u32) -> u32;
        pub fn hidSendVibrationValues(handles: *const u32, values: *const HidVibrationValue, count: i32) -> u32;
        pub fn hidGetSixAxisSensorHandles(handles: *mut u32, total_handles: i32, id: u32, style: u32) -> u32;
        pub fn hidStartSixAxisSensor(handle: u32) -> u32;
        pub fn hidStopSixAxisSensor(handle: u32) -> u32;
        pub fn hidGetSixAxisSensorStates(handle: u32, states: *mut HidSixAxisSensorState, count: usize) -> usize;
        pub fn hidInitializeTouchScreen();
        pub fn hidGetTouchScreenStates(states: *mut HidTouchScreenState, count: usize) -> usize;
    }
}

#[cfg(target_os = "horizon")]
#[derive(Default)]
struct Slot {
    pad: PadState,
    /// `(id, style)` the handles were made for, the handles, how many.
    vibration: Option<((u32, u32), [u32; 2], usize)>,
    /// `(id, style)` the started sensor belongs to, its handle.
    six_axis: Option<((u32, u32), u32)>,
}

/// Up to eight player slots read through libnx's pad API.
#[cfg(target_os = "horizon")]
pub struct Pads {
    slots: Vec<Slot>,
}

#[cfg(target_os = "horizon")]
impl Pads {
    /// Declares `max_players` players (1–8) with the standard styles and the handheld.
    pub fn init(max_players: u8) -> anyhow::Result<Self> {
        if !(1..=8).contains(&max_players) {
            anyhow::bail!("configure {max_players} pads: libnx takes 1 to 8");
        }
        // SAFETY: 1..=8 players, the range in which padConfigureInput does not abort.
        unsafe { ffi::padConfigureInput(u32::from(max_players), STYLE_SET_STANDARD) };
        let mut slots: Vec<Slot> = (0..max_players).map(|_| Slot::default()).collect();
        for (i, slot) in slots.iter_mut().enumerate() {
            let mask = if i == 0 { 1 | (1 << NPAD_ID_HANDHELD) } else { 1 << i };
            // SAFETY: `slot.pad` is a valid PadState that padInitializeWithMask overwrites.
            unsafe { ffi::padInitializeWithMask(&mut slot.pad, mask) };
        }
        Ok(Self { slots })
    }

    /// Reads every slot once; call once a frame before [`Pads::sample`].
    pub fn update(&mut self) {
        for slot in &mut self.slots {
            // SAFETY: a PadState initialised by padInitializeWithMask.
            unsafe { ffi::padUpdate(&mut slot.pad) };
        }
    }

    pub fn sample(&self, slot: usize) -> Option<PadRaw> {
        self.slots.get(slot).map(|s| PadRaw::from_state(&s.pad))
    }

    /// Sends `[left, right]` to the slot's actuators. A slot with no pad drops the values.
    pub fn send_vibration(&mut self, slot: usize, values: [Vibration; 2]) -> anyhow::Result<()> {
        let Some(s) = self.slots.get_mut(slot) else {
            anyhow::bail!("send vibration to slot {slot}: no such slot");
        };
        let Some(target @ (id, style)) = npad_target(slot, &s.pad) else {
            return Ok(());
        };
        // TODO(verify): two handles come back `[left, right]`, as libnx's examples use.
        if s.vibration.map(|(t, ..)| t) != Some(target) {
            let n = if matches!(style, STYLE_JOY_LEFT | STYLE_JOY_RIGHT) {
                1
            } else {
                2
            };
            let mut handles = [0u32; 2];
            // SAFETY: `handles` has room for `n` ≤ 2 handles.
            let rc = unsafe { ffi::hidInitializeVibrationDevices(handles.as_mut_ptr(), n as i32, id, style) };
            if rc != 0 {
                return Err(crate::nx_error("initialize vibration devices", rc));
            }
            s.vibration = Some((target, handles, n));
        }
        let Some((_, handles, n)) = s.vibration else {
            return Ok(());
        };
        let (hv, _) = vibration_values(style, values);
        // SAFETY: `handles` and `hv` both hold at least `n` entries.
        let rc = unsafe { ffi::hidSendVibrationValues(handles.as_ptr(), hv.as_ptr(), n as i32) };
        if rc != 0 {
            return Err(crate::nx_error("send vibration values", rc));
        }
        Ok(())
    }

    /// The slot's latest motion sample, starting its sensor on first use. A Joy-Con pair reports
    /// the right Joy-Con.
    pub fn sixaxis(&mut self, slot: usize) -> Option<SixAxis> {
        let s = self.slots.get_mut(slot)?;
        let target @ (id, style) = npad_target(slot, &s.pad)?;
        if s.six_axis.map(|(t, _)| t) != Some(target) {
            if let Some((_, old)) = s.six_axis.take() {
                // SAFETY: a handle this slot started.
                unsafe { ffi::hidStopSixAxisSensor(old) };
            }
            let n: i32 = if style == STYLE_JOY_DUAL { 2 } else { 1 };
            let mut handles = [0u32; 2];
            // SAFETY: `handles` has room for `n` ≤ 2 handles.
            if unsafe { ffi::hidGetSixAxisSensorHandles(handles.as_mut_ptr(), n, id, style) } != 0 {
                return None;
            }
            // TODO(verify): a pair's handles come back `[left, right]`, as libnx's examples use.
            let handle = handles[n as usize - 1];
            // SAFETY: a handle hid just returned.
            if unsafe { ffi::hidStartSixAxisSensor(handle) } != 0 {
                return None;
            }
            s.six_axis = Some((target, handle));
        }
        let (_, handle) = s.six_axis?;
        let mut state = HidSixAxisSensorState::default();
        // SAFETY: room for one state.
        let n = unsafe { ffi::hidGetSixAxisSensorStates(handle, &mut state, 1) };
        if n == 0 {
            return None;
        }
        SixAxis::from_state(&state)
    }
}

#[cfg(target_os = "horizon")]
impl Drop for Pads {
    fn drop(&mut self) {
        for (_, handle) in self.slots.iter().filter_map(|s| s.six_axis) {
            // SAFETY: a handle this value started.
            unsafe { ffi::hidStopSixAxisSensor(handle) };
        }
    }
}

/// The fingers on the touch screen now, replacing `out`'s contents.
#[cfg(target_os = "horizon")]
pub fn touches(out: &mut Vec<Touch>) {
    static INIT: std::sync::Once = std::sync::Once::new();
    // SAFETY: no arguments; libnx initialises the touch screen's shared memory once.
    INIT.call_once(|| unsafe { ffi::hidInitializeTouchScreen() });
    out.clear();
    let mut state = HidTouchScreenState::default();
    // SAFETY: room for one state.
    if unsafe { ffi::hidGetTouchScreenStates(&mut state, 1) } == 0 {
        return;
    }
    let n = state.count.clamp(0, 16) as usize;
    out.extend(state.touches[..n].iter().map(Touch::from));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad(style_set: u32, active_id_mask: u8, handheld: bool) -> PadState {
        PadState {
            active_id_mask,
            active_handheld: handheld,
            style_set,
            attributes: NPAD_ATTRIBUTE_IS_CONNECTED,
            buttons_cur: 0b101,
            sticks: [
                HidAnalogStickState { x: JOYSTICK_MAX, y: -5 },
                HidAnalogStickState { x: -JOYSTICK_MAX, y: 7 },
            ],
            ..PadState::default()
        }
    }

    #[test]
    fn pad_raw_reads_the_state() {
        let raw = PadRaw::from_state(&pad(STYLE_FULL_KEY, 1, false));
        assert!(raw.connected);
        assert_eq!(raw.style, Style::FullKey);
        assert_eq!(raw.buttons, 0b101);
        assert_eq!((raw.lx, raw.ly, raw.rx, raw.ry), (JOYSTICK_MAX, -5, -JOYSTICK_MAX, 7));
        assert!(!PadRaw::from_state(&PadState::default()).connected);
    }

    #[test]
    fn style_prefers_handheld_then_full_key() {
        assert_eq!(Style::from_set(STYLE_FULL_KEY, true), Style::Handheld);
        assert_eq!(Style::from_set(STYLE_FULL_KEY | STYLE_JOY_DUAL, false), Style::FullKey);
        assert_eq!(Style::from_set(STYLE_JOY_RIGHT, false), Style::JoyRight);
        assert_eq!(Style::from_set(1 << 5, false), Style::Other);
    }

    #[test]
    fn rumble_and_motion_address_the_connected_controller() {
        assert_eq!(
            npad_target(0, &pad(STYLE_FULL_KEY, 1, true)),
            Some((NPAD_ID_HANDHELD, STYLE_HANDHELD))
        );
        assert_eq!(
            npad_target(2, &pad(STYLE_JOY_DUAL, 1 << 2, false)),
            Some((2, STYLE_JOY_DUAL))
        );
        assert_eq!(npad_target(1, &pad(STYLE_JOY_DUAL, 0, false)), None);
        assert_eq!(npad_target(1, &pad(1 << 5, 1 << 1, false)), None);
    }

    #[test]
    fn vibration_clamps_and_picks_the_side() {
        let hot = Vibration {
            amp_low: 2.0,
            freq_low: 160.0,
            amp_high: f32::NAN,
            freq_high: 320.0,
        };
        let cold = Vibration { amp_low: -1.0, ..hot };
        let (v, n) = vibration_values(STYLE_JOY_DUAL, [hot, cold]);
        assert_eq!(n, 2);
        assert_eq!(
            v[0],
            HidVibrationValue {
                amp_low: 1.0,
                freq_low: 160.0,
                amp_high: 0.0,
                freq_high: 320.0
            }
        );
        assert_eq!(v[1].amp_low, 0.0);
        let (v, n) = vibration_values(STYLE_JOY_RIGHT, [hot, cold]);
        assert_eq!((n, v[0].amp_low), (1, 0.0));
    }

    #[test]
    fn touch_and_motion_convert() {
        let t = HidTouchState {
            finger_id: 3,
            x: 640,
            y: 360,
            diameter_x: 20,
            diameter_y: 22,
            ..HidTouchState::default()
        };
        assert_eq!(
            Touch::from(&t),
            Touch {
                x: 640,
                y: 360,
                id: 3,
                diameter_x: 20,
                diameter_y: 22
            }
        );
        let mut s = HidSixAxisSensorState {
            sampling_number: 9,
            acceleration: HidVector {
                x: 0.0,
                y: 0.0,
                z: -1.0,
            },
            angular_velocity: HidVector { x: 0.5, y: 0.0, z: 0.0 },
            ..HidSixAxisSensorState::default()
        };
        assert_eq!(SixAxis::from_state(&s), None);
        s.attributes = SIX_AXIS_ATTRIBUTE_IS_CONNECTED;
        let m = SixAxis::from_state(&s).unwrap();
        assert_eq!((m.accel, m.gyro, m.timestamp), ([0.0, 0.0, -1.0], [0.5, 0.0, 0.0], 9));
    }
}
