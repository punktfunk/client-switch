#!/usr/bin/env bash
# Runs a command in the toolchain image on a copy of this tree: scripts/docker.sh <command…>
# The tree goes in over stdin (git's file list) instead of a bind mount, which can hang Docker
# Desktop on an external disk. target/ and cargo's registry live in volumes; dist/ comes back out.
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=scripts/pins.env
source "$root/scripts/pins.env"
vol=${PF_NX_VOLUME:-pf-nx}
cd "$root"

# Tracked and untracked files, less the ones deleted from the work tree.
git ls-files -co --exclude-standard -z | while IFS= read -r -d '' f; do [[ ! -e $f ]] || printf '%s\0' "$f"; done |
    COPYFILE_DISABLE=1 tar --null -T - -czf - |
    docker run --rm -i -v "$vol-src:/w" alpine:3 sh -c \
        'find /w -mindepth 1 -maxdepth 1 ! -name target -exec rm -rf {} + && tar -xzf - -C /w'
# The archives every build reads (Skia, NVK, the patched shared crates) live in one volume at
# /opt/pf-nx, shared by every source volume: PF_NX_VOLUME separates parallel builds, not these.
docker run --rm -v "$vol-src:/w" -v "$vol-target:/w/target" -v "$vol-cargo:/usr/local/cargo/registry" \
    -v "$vol-git:/usr/local/cargo/git" -v "${PF_NX_TOOLCHAINS_VOLUME:-pf-nx-toolchains}:/opt/pf-nx" \
    -w /w -e PKG_VERSION "$NX_IMAGE" "$@"
if docker run --rm -v "$vol-src:/w" alpine:3 test -d /w/dist; then
    docker run --rm -v "$vol-src:/w" alpine:3 tar -cf - -C /w dist | tar -xf - -C "$root"
fi
