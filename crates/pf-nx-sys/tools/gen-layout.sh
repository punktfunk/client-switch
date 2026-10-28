#!/usr/bin/env bash
# Prints the size, alignment and field offsets bindgen derives from the libnx headers for every
# struct tests/layout.rs pins, to set beside the hand-derived numbers there. Runs in the
# toolchain image: needs bindgen (bindgen-cli), libclang and devkitPro under $DEVKITPRO.
set -euo pipefail
DKP="${DEVKITPRO:-/opt/devkitpro}"
TYPES='SocketInitConfig|PadState|HidAnalogStickState|HidTouchState|HidTouchScreenState|HidSixAxisSensorState|HidVibrationValue|AppletHookCookie|Framebuffer|NvMap|AudioOutBuffer|ClkrstSession|SetSysFirmwareVersion'

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
printf '#include <switch.h>\n' > "$work/wrapper.h"

bindgen "$work/wrapper.h" --allowlist-type "^($TYPES)\$" -- \
    --target=aarch64-none-elf -D__SWITCH__ \
    -isystem "$DKP/libnx/include" -isystem "$DKP/devkitA64/aarch64-none-elf/include" \
    | sed -nE 's/.*\["(Size of|Alignment of|Offset of field:) ([^"]+)"\].* - ([0-9]+)usize\].*/\1 \2 = \3/p'
