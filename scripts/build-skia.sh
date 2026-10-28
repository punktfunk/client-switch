#!/usr/bin/env bash
# Builds the Skia archive skia-bindings links for the console: scripts/build-skia.sh [raster|vulkan]
# Runs in the toolchain image (scripts/docker.sh). skia-bindings' own offline build compiles Skia
# with devkitA64 (toolchain/skia/) and runs bindgen; this packs its output where scripts/cargo-nx.sh
# points SKIA_BINARIES_URL, under the name skia-bindings computes for the target and feature set.
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=scripts/pins.env
source "$root/scripts/pins.env"
dkp=${DEVKITPRO:-/opt/devkitpro}
export PATH="$dkp/devkitA64/bin:$PATH"
cd "$root"

# The key's feature list is skia-bindings' sorted feature names (pdf and jpeg are skia-safe
# defaults); `binds` are the sources skia-bindings runs bindgen on for those features.
case ${1:-raster} in
    raster)
        features='"textlayout"' keyed=jpegd-jpege-pdf-textlayout
        binds=(bindings.cpp shaper.cpp paragraph.cpp) ;;
    vulkan)
        features='"textlayout", "vulkan", "ganesh"' keyed=ganesh-jpegd-jpege-pdf-textlayout-vulkan
        binds=(bindings.cpp shaper.cpp paragraph.cpp vulkan.cpp gpu.cpp ganesh.cpp) ;;
    *) echo "usage: scripts/build-skia.sh [raster|vulkan]" >&2; exit 2 ;;
esac
ver=$(grep -A1 '^name = "skia-bindings"$' Cargo.lock | sed -n 's/^version = "\(.*\)"/\1/p')
work=$root/target/skia
crate=$work/${1:-raster}

# A crate of its own selects exactly this feature set; a workspace member would unify others in.
mkdir -p "$crate/src"
: >"$crate/src/lib.rs"
cp Cargo.lock "$crate/"
cat >"$crate/Cargo.toml" <<EOF
[package]
name = "skia-archive"
version = "0.0.0"
edition = "2021"

[dependencies]
skia-bindings = { version = "=$ver", features = [$features] }

[workspace]
EOF
cargo "+$RUST_NIGHTLY" fetch -q --manifest-path "$crate/Cargo.toml"
reg=$(echo "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/"skia-bindings-$ver")
hash=$(sed -n 's/.*"sha1": "\(.\{20\}\).*/\1/p' "$reg/.cargo_vcs_info.json")
rev=$(sed -n 's/^skia = "\(.*\)"$/\1/p' "$reg/Cargo.toml")
key=$hash-aarch64-nintendo-horizon-$keyed
dest=${PF_NX_TOOLCHAINS:-/opt/pf-nx}/skia/$ver
if [[ -f $dest/skia-binaries-$key.tar.gz ]]; then
    echo "==> skia $ver $key (built)"
    exit 0
fi

# rust-skia's Skia fork at the crate's pin, with the third-party trees its DEPS names and gn.
src=$work/skia-$rev
if [[ ! -x $src/bin/gn ]]; then
    rm -rf "$src"
    mkdir -p "$src"
    curl -fsSL "https://codeload.github.com/rust-skia/skia/tar.gz/$rev" |
        tar -xz -C "$src" --strip-components=1 --exclude='*/infra'
    (cd "$src" && GIT_SYNC_DEPS_QUIET=1 GIT_SYNC_DEPS_SKIP_EMSDK=1 python3 tools/git-sync-deps)
fi
# The tree keeps a copy of the patch it carries, so a changed patch replaces the old one.
if ! cmp -s "$src/.horizon.patch" toolchain/skia/horizon.patch; then
    [[ ! -f $src/.horizon.patch ]] || git -C "$src" apply -R "$src/.horizon.patch"
    git -C "$src" apply "$root/toolchain/skia/horizon.patch"
    cp toolchain/skia/horizon.patch "$src/.horizon.patch"
fi

# Half the cores, leaving memory for GCC at -O3 on Skia's larger files and for other builds.
printf '#!/bin/sh\nexec ninja -j%s "$@"\n' "$(( $(nproc) / 2 ))" >"$work/ninja"
chmod +x "$work/ninja"
# bindgen parses Skia's headers as aarch64 Linux: the image's clang 14 cannot read devkitA64's
# libstdc++ 16. The layout check below holds GCC's layouts for the console to what it computed.
# The hyphenated name outranks the one scripts/cargo-nx.sh exports, and lands after skia-bindings'.
env SKIA_SOURCE_DIR="$src" SKIA_NINJA_COMMAND="$work/ninja" \
    SKIA_GN_ARGS="$(<toolchain/skia/args.gn)" \
    CLANGCC="$root/toolchain/skia/cc" CLANGCXX="$root/toolchain/skia/c++" \
    "BINDGEN_EXTRA_CLANG_ARGS_aarch64-nintendo-horizon=--target=aarch64-unknown-linux-gnu" \
    bash scripts/cargo-nx.sh check --manifest-path "$crate/Cargo.toml"

out=$(echo "$crate"/target/aarch64-nintendo-horizon/release/build/skia-bindings/*/out/skia)
[[ -f $out/bindings.rs ]] || { echo "no bindings.rs under $out" >&2; exit 1; }
# shellcheck disable=SC2046 # one define per word
python3 -I toolchain/skia/layout-check.py "$out/bindings.rs" "${binds[@]/#/$reg/src/}" -- \
    toolchain/skia/c++ -std=c++20 -fno-rtti -I"$src" $(<"$out/skia-defines.txt")

stage=$work/stage/skia-binaries
rm -rf "$stage"
mkdir -p "$stage" "$dest"
cp "$out"/lib{skia,skia-bindings,skshaper,skparagraph,skunicode_core,skunicode_icu}.a \
    "$out/bindings.rs" "$stage/"
cp "$src/LICENSE" "$stage/LICENSE_SKIA"
toolchain/skia/cc -O2 -c toolchain/skia/compat.c -o "$work/compat.o"
aarch64-none-elf-ar rs "$stage/libskia.a" "$work/compat.o"
printf %s "$ver" >"$stage/tag.txt"
printf %s "$key" >"$stage/key.txt"
# A build that reads the archive mid-write would take it for corrupt and compile Skia itself.
tar -czf "$dest/.skia-binaries-$key.tar.gz" -C "$stage/.." skia-binaries
mv "$dest/.skia-binaries-$key.tar.gz" "$dest/skia-binaries-$key.tar.gz"
ls -l "$dest"
