//! Every `#[repr(C)]` struct the crate hands libnx or the shim, pinned at the size, alignment
//! and offsets derived by hand from the libnx 4.12.0 headers (`tools/gen-layout.sh` prints the
//! same through bindgen). Fields are fixed-width integers, floats, `bool` and pointers, so any
//! LP64 host lays them out as aarch64 does, and the Mac runs these.
#![cfg(target_pointer_width = "64")]

use std::mem::{align_of, offset_of, size_of};

use pf_nx_sys::applet::AppletHookCookie;
use pf_nx_sys::audout::AudioOutBuffer;
use pf_nx_sys::av::{PictureMeta, RawStats};
use pf_nx_sys::clk::ClkrstSession;
use pf_nx_sys::fb::RawFramebuffer;
use pf_nx_sys::hid::*;
use pf_nx_sys::mem::MemoryInfo;
use pf_nx_sys::setsys::SetSysFirmwareVersion;
use pf_nx_sys::socket::SocketInitConfig;

fn layout<T>() -> (usize, usize) {
    (size_of::<T>(), align_of::<T>())
}

#[test]
fn socket_init_config() {
    // 4 × u32 TCP, 2 × u32 UDP, sb_efficiency, num_bsd_sessions, BsdServiceType (u32) → 36.
    assert_eq!(layout::<SocketInitConfig>(), (36, 4));
    assert_eq!(offset_of!(SocketInitConfig, udp_tx_buf_size), 16);
    assert_eq!(offset_of!(SocketInitConfig, sb_efficiency), 24);
    assert_eq!(offset_of!(SocketInitConfig, num_bsd_sessions), 28);
    assert_eq!(offset_of!(SocketInitConfig, bsd_service_type), 32);
}

#[test]
fn memory_info() {
    // u64 addr, u64 size, then six u32: type, attr, perm, ipc_refcount, device_refcount,
    // padding → 40, align 8.
    assert_eq!(layout::<MemoryInfo>(), (40, 8));
    assert_eq!(offset_of!(MemoryInfo, kind), 16);
    assert_eq!(offset_of!(MemoryInfo, attr), 20);
    assert_eq!(offset_of!(MemoryInfo, perm), 24);
    assert_eq!(offset_of!(MemoryInfo, padding), 36);
}

#[test]
fn pad_state() {
    // u8, u8, bool, bool, u32 style_set, u32 attributes, (4) u64 cur, u64 old, 2 × {s32,s32},
    // 2 × u32 → 56, align 8.
    assert_eq!(layout::<PadState>(), (56, 8));
    assert_eq!(offset_of!(PadState, active_handheld), 3);
    assert_eq!(offset_of!(PadState, style_set), 4);
    assert_eq!(offset_of!(PadState, attributes), 8);
    assert_eq!(offset_of!(PadState, buttons_cur), 16);
    assert_eq!(offset_of!(PadState, buttons_old), 24);
    assert_eq!(offset_of!(PadState, sticks), 32);
    assert_eq!(offset_of!(PadState, gc_triggers), 48);
    assert_eq!(layout::<HidAnalogStickState>(), (8, 4));
}

#[test]
fn touch_states() {
    // HidTouchState: u64 delta_time + 8 × u32 → 40, align 8.
    assert_eq!(layout::<HidTouchState>(), (40, 8));
    assert_eq!(offset_of!(HidTouchState, finger_id), 12);
    assert_eq!(offset_of!(HidTouchState, x), 16);
    assert_eq!(offset_of!(HidTouchState, diameter_x), 24);
    // HidTouchScreenState: u64 sampling_number, s32 count, u32 reserved, 16 × 40 → 656.
    assert_eq!(layout::<HidTouchScreenState>(), (656, 8));
    assert_eq!(offset_of!(HidTouchScreenState, count), 8);
    assert_eq!(offset_of!(HidTouchScreenState, touches), 16);
}

#[test]
fn six_axis_state() {
    // u64 delta_time, u64 sampling_number, 3 × HidVector (12), 3×3 f32 (36), u32, u32 → 96.
    assert_eq!(layout::<HidSixAxisSensorState>(), (96, 8));
    assert_eq!(offset_of!(HidSixAxisSensorState, acceleration), 16);
    assert_eq!(offset_of!(HidSixAxisSensorState, angular_velocity), 28);
    assert_eq!(offset_of!(HidSixAxisSensorState, angle), 40);
    assert_eq!(offset_of!(HidSixAxisSensorState, direction), 52);
    assert_eq!(offset_of!(HidSixAxisSensorState, attributes), 88);
}

#[test]
fn vibration_value() {
    // 4 × f32 → 16, align 4.
    assert_eq!(layout::<HidVibrationValue>(), (16, 4));
    assert_eq!(offset_of!(HidVibrationValue, freq_high), 12);
}

#[test]
fn opaque_libnx_structs() {
    // AppletHookCookie: next, callback, param → 3 pointers, 24.
    assert_eq!(layout::<AppletHookCookie>(), (24, 8));
    // Framebuffer: NWindow* (8), NvMap {u32 ×3, (4), void*, NvKind, bool, bool, (2)} = 32,
    // buf, buf_linear (16), 5 × u32 (20), bool (1), padding (3) → 80, align 8.
    assert_eq!(layout::<RawFramebuffer>(), (80, 8));
    // ClkrstSession { Service { Handle, u32, u32, u16 } } → 14 + 2 padding = 16, align 4.
    assert_eq!(layout::<ClkrstSession>(), (16, 4));
}

#[test]
fn audio_out_buffer() {
    // next*, buffer*, u64 buffer_size, u64 data_size, u64 data_offset → 40.
    assert_eq!(layout::<AudioOutBuffer>(), (40, 8));
    assert_eq!(offset_of!(AudioOutBuffer, buffer), 8);
    assert_eq!(offset_of!(AudioOutBuffer, data_size), 24);
}

#[test]
fn firmware_version() {
    // 8 × u8, char[0x20], char[0x40], char[0x18], char[0x80] → 0x100, align 1.
    assert_eq!(layout::<SetSysFirmwareVersion>(), (0x100, 1));
    assert_eq!(offset_of!(SetSysFirmwareVersion, platform), 8);
    assert_eq!(offset_of!(SetSysFirmwareVersion, display_version), 0x68);
    assert_eq!(offset_of!(SetSysFirmwareVersion, display_title), 0x80);
}

#[test]
fn shim_structs() {
    // PfAvPictureMeta: u32, u32, u64 → 16. PfAvStats: u64, 7 × 32-bit → 36, padded to 40.
    assert_eq!(layout::<PictureMeta>(), (16, 8));
    assert_eq!(layout::<RawStats>(), (40, 8));
    assert_eq!(offset_of!(RawStats, in_flight), 8);
    assert_eq!(offset_of!(RawStats, nvdec_khz), 32);
}
