#!/usr/bin/env bash
# Builds NVK for the console into the archives volume at /opt/pf-nx/nvk/lib/libvulkan_bh.a:
#   [JOBS=n] [PF_NX_NVK_WORK=dir] scripts/build-nvk.sh
# bevy-horizon's recipe at BEVY_HORIZON_REV: its toolchain image on NX_IMAGE (build that first),
# build_nvk.sh at the pinned mesa-switch commit with its patches, then localize_nvk.sh.
# docs/pins.md has the result.
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=scripts/pins.env
source "$root/scripts/pins.env"
mesa_repo=${MESA_SWITCH_REPO:-https://github.com/danfromtico/mesa-switch}
mesa_rev=${MESA_SWITCH_REV:-606441c90e459028a50fca352daebfc20211f890}
# bevy-horizon's scripts bind-mount the work tree, which hangs Docker Desktop on an external disk.
# One tree per pin pair, so a bump starts clean and a rerun is incremental.
work=${PF_NX_NVK_WORK:-${TMPDIR:-/tmp}/pf-nx-nvk}/${BEVY_HORIZON_REV:0:9}-${mesa_rev:0:9}
bh=$work/bevy-horizon
export JOBS=${JOBS:-$(docker info --format '{{.NCPU}}')}

# checkout <dir> <repo> <rev>: one commit, shallow; kept when HEAD is already that commit.
checkout() {
    [[ $(git -C "$1" rev-parse -q --verify HEAD 2>/dev/null) == "$3" ]] && return
    rm -rf "$1"
    git init -q "$1"
    git -C "$1" remote add origin "$2"
    git -C "$1" fetch -q --depth 1 origin "$3"
    git -C "$1" checkout -q FETCH_HEAD
}
checkout "$bh" "$BEVY_HORIZON_REPO" "$BEVY_HORIZON_REV"
checkout "$work/mesa-switch" "$mesa_repo" "$mesa_rev"

# The Containerfile on NX_IMAGE, less its dkp-pacman RUN: NX_IMAGE holds every package it names,
# and devkitPro's server blocks CI. Its rustup names the x86_64 toolchain; uname -m fits arm64.
# shellcheck disable=SC2016 # $(uname -m) expands in the image's RUN.
sed -e "s|^FROM .*|FROM $NX_IMAGE|" -e '/^RUN dkp-pacman/,/-Scc/d' \
    -e 's/x86_64-unknown-linux-gnu/$(uname -m)-unknown-linux-gnu/g' \
    "$bh/Containerfile" | docker build -t bevy-horizon-toolchain:local -
"$bh/scripts/build_nvk.sh" "$work/mesa-switch"
"$bh/scripts/localize_nvk.sh"

COPYFILE_DISABLE=1 tar -czf - -C "$bh/nvk" BUILD_INFO lib/libvulkan_bh.a lib/undefined_bh.txt |
    docker run --rm -i -v "${PF_NX_TOOLCHAINS_VOLUME:-pf-nx-toolchains}:/opt/pf-nx" alpine:3 sh -c \
        'rm -rf /opt/pf-nx/nvk && mkdir -p /opt/pf-nx/nvk && tar -xzf - -C /opt/pf-nx/nvk &&
         ls -l /opt/pf-nx/nvk/lib && sha256sum /opt/pf-nx/nvk/lib/libvulkan_bh.a'
