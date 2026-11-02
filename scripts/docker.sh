#!/usr/bin/env bash
# Runs a command in the toolchain image on a copy of this tree: scripts/docker.sh <command…>
# PF_NX_IMAGE runs it in another image instead (HOST_IMAGE, the dev box's).
# The tree goes in over stdin (git's file list) instead of a bind mount, which can hang Docker
# Desktop on an external disk. target/ and cargo's registry live in volumes; dist/ comes back out.
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=scripts/pins.env
source "$root/scripts/pins.env"
vol=${PF_NX_VOLUME:-pf-nx}
cd "$root"

# The version and build id the title carries (core::build_id, the NACP). A v* tag on HEAD gives
# its version; any other commit the crate's plus the commit, which fits the NACP's 16 bytes.
if [[ -z ${PKG_VERSION:-} ]]; then
    if tag=$(git describe --tags --exact-match --match 'v*' 2>/dev/null); then
        PKG_VERSION=${tag#v}
    else
        PKG_VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)+g$(git rev-parse --short=7 HEAD)
    fi
fi
export PKG_VERSION
export PF_CORE_REV=${PUNKTFUNK_REV:0:9}
PF_MESA_PIN=$(sed -n 's/^mesa_rev=\${MESA_SWITCH_REV:-\([0-9a-f]\{9\}\).*/\1/p' scripts/build-nvk.sh)
export PF_MESA_PIN

# Tracked and untracked files, less the ones deleted from the work tree.
git ls-files -co --exclude-standard -z | while IFS= read -r -d '' f; do [[ ! -e $f ]] || printf '%s\0' "$f"; done |
    COPYFILE_DISABLE=1 tar --null -T - -czf - |
    docker run --rm -i -v "$vol-src:/w" alpine:3 sh -c \
        'find /w -mindepth 1 -maxdepth 1 ! -name target -exec rm -rf {} + && tar -xzf - -C /w'
# The archives every build reads (Skia, NVK) live in one volume at
# /opt/pf-nx, shared by every source volume: PF_NX_VOLUME separates parallel builds, not these.
docker run --rm -v "$vol-src:/w" -v "$vol-target:/w/target" -v "$vol-cargo:/usr/local/cargo/registry" \
    -v "$vol-git:/usr/local/cargo/git" -v "${PF_NX_TOOLCHAINS_VOLUME:-pf-nx-toolchains}:/opt/pf-nx" \
    -w /w -e PKG_VERSION -e PF_CORE_REV -e PF_MESA_PIN "${PF_NX_IMAGE:-$NX_IMAGE}" "$@"
if docker run --rm -v "$vol-src:/w" alpine:3 test -d /w/dist; then
    docker run --rm -v "$vol-src:/w" alpine:3 tar -cf - -C /w dist | tar -xf - -C "$root"
fi
