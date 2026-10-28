#!/usr/bin/env bash
# Cross-builds for the Switch: scripts/cargo-nx.sh [build|check|clippy] [cargo args…]
# std is built from source on the pinned nightly for toolchain/aarch64-nintendo-horizon.json, on
# the libc leg; C and C++ build with devkitA64 (.cargo/config.toml). Runs in the toolchain image.
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=scripts/pins.env
source "$root/scripts/pins.env"
dkp=${DEVKITPRO:-/opt/devkitpro}
[[ -x $dkp/devkitA64/bin/aarch64-none-elf-gcc ]] || { echo "no devkitA64 under $dkp: run this in the toolchain image (scripts/docker.sh)" >&2; exit 2; }
# cc-rs resolves the bare compiler names in .cargo/config.toml through PATH; a login shell's
# /etc/profile drops the image's.
export PATH="$dkp/devkitA64/bin:$dkp/tools/bin:$PATH"

cmd=${1:-build}
shift || true
cd "$root"

# std builds as its own workspace, locked to crates.io's libc, whose Horizon layouts are the 3DS's.
# The config's [patch] reaches that workspace; this moves its lock onto the leg.
std_lib="$(rustc "+$RUST_NIGHTLY" --print sysroot)/lib/rustlib/src/rust/library"
cargo "+$RUST_NIGHTLY" update -q -p libc --manifest-path "$std_lib/Cargo.toml"
if grep -A2 '^name = "libc"' "$std_lib/Cargo.lock" | grep -q '^source = '; then
    echo "std would build against crates.io's libc, not toolchain/patches/libc" >&2
    exit 1
fi

t=aarch64_nintendo_horizon
export "CMAKE_TOOLCHAIN_FILE_$t=$dkp/cmake/Switch.cmake"
# bindgen parses C headers for the console: newlib from devkitA64, sockets from libnx.
export "BINDGEN_EXTRA_CLANG_ARGS_$t=--target=aarch64-none-elf -D__SWITCH__ -isystem $dkp/libnx/include -isystem $dkp/devkitA64/aarch64-none-elf/include"

# aws-lc takes its own CFLAGS whole. No BIO sockets (no <sys/un.h>); toolchain/include has the
# <sys/termios.h> newlib lacks; CPU features from -march, as no OS reports them; OPENSSL_WASM
# only selects getentropy (the probe's `_getentropy_r`) over /dev/urandom (rand_extra/internal.h).
export "AWS_LC_SYS_CFLAGS_$t=-march=armv8-a+crc+crypto -mtune=cortex-a57 -mtp=soft -fPIE -D__SWITCH__ -isystem $dkp/libnx/include -isystem $root/toolchain/include -D_DEFAULT_SOURCE -DOPENSSL_NO_SOCK -DOPENSSL_STATIC_ARMCAP -DOPENSSL_WASM"

# The archives under /opt/pf-nx (scripts/docker.sh mounts the volume): Skia as skia-bindings
# expects it (scripts/build-skia.sh), NVK's localized library (scripts/build-nvk.sh), and, until
# plan §7's monorepo PRs land, the pinned shared crates with their Horizon cfgs (PF_NX_MONO).
tc=${PF_NX_TOOLCHAINS:-/opt/pf-nx}
export SKIA_BINARIES_URL="${SKIA_BINARIES_URL:-file://$tc/skia/{tag}/skia-binaries-{key}.tar.gz}"
export PF_NX_NVK_LIB="${PF_NX_NVK_LIB:-$tc/nvk/lib}"
mono=${PF_NX_MONO:-$tc/mono}
patches=()
if [[ -d $mono/crates ]]; then
    for p in core/punktfunk-core client/pf-client-core client/pf-client-video client/pf-console-ui; do
        patches+=(--config "patch.\"https://git.unom.io/unom/punktfunk\".${p##*/}.path=\"$mono/crates/$p\"")
    done
fi

exec cargo "+$RUST_NIGHTLY" "$cmd" --release -Z build-std=core,alloc,std,panic_abort -Z json-target-spec \
    --target "$root/toolchain/aarch64-nintendo-horizon.json" "${patches[@]}" "$@"
