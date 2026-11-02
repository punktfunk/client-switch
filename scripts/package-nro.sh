#!/usr/bin/env bash
# Wraps a built console ELF as an NRO with a NACP and libnx's default icon:
#   scripts/package-nro.sh <bin> [title]   →   dist/<title>.nro (the title defaults to <bin>)
# The version is PKG_VERSION, else the package's own. Runs in the toolchain image.
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
bin=${1:?usage: scripts/package-nro.sh <bin> [title]}
title=${2:-$bin}
elf=$root/target/aarch64-nintendo-horizon/release/$bin
[[ -f $elf ]] || { echo "no $elf: build it with scripts/cargo-nx.sh build -p <package>" >&2; exit 2; }
version=${PKG_VERSION:-$(cd "$root" && cargo pkgid -p "$bin" | sed 's/.*[#@]//')}

mkdir -p "$root/dist"
# Without --nacp and --icon the NRO has no asset section, which hbmenu forwarders refuse.
nacptool --create "$title" punktfunk "$version" "$root/dist/$title.nacp"
elf2nro "$elf" "$root/dist/$title.nro" --nacp="$root/dist/$title.nacp" --icon="${DEVKITPRO:-/opt/devkitpro}/libnx/default_icon.jpg"
# The ELF keeps the symbols and line tables the NRO drops: a crash report's offsets resolve
# against it (`aarch64-none-elf-addr2line -fipe dist/<title>.elf <offset>`).
cp "$elf" "$root/dist/$title.elf"
# The notices pf-nx-app's build wrote for this link; the NRO's licence (GPL) travels with it.
if [[ -f $root/THIRD-PARTY-NOTICES.txt ]]; then cp "$root/THIRD-PARTY-NOTICES.txt" "$root/dist/"; fi
ls -la "$root/dist/$title.nro" "$root/dist/$title.elf"
