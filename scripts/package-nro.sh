#!/usr/bin/env bash
# Wraps a built console ELF as an NRO with a NACP and packaging/icon.jpg:
#   scripts/package-nro.sh <bin> [name] [title]   →   dist/<name>.nro
# The name (default <bin>) names the files; the title (default the name) is what the Homebrew Menu
# shows. The version is PKG_VERSION, else the package's own. Runs in the toolchain image.
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
bin=${1:?usage: scripts/package-nro.sh <bin> [name] [title]}
name=${2:-$bin}
title=${3:-$name}
elf=$root/target/aarch64-nintendo-horizon/release/$bin
[[ -f $elf ]] || { echo "no $elf: build it with scripts/cargo-nx.sh build -p <package>" >&2; exit 2; }
version=${PKG_VERSION:-$(cd "$root" && cargo pkgid -p "$bin" | sed 's/.*[#@]//')}

mkdir -p "$root/dist"
# Without --nacp and --icon the NRO has no asset section, which hbmenu forwarders refuse.
# The icon is icon_large.png scaled to the 256x256 JPEG the NACP takes; no larger art exists.
nacptool --create "$title" broslife "$version" "$root/dist/$name.nacp"
elf2nro "$elf" "$root/dist/$name.nro" --nacp="$root/dist/$name.nacp" --icon="$root/packaging/icon.jpg"
# The ELF keeps the symbols and line tables the NRO drops: a crash report's offsets resolve
# against it (`aarch64-none-elf-addr2line -fipe dist/<name>.elf <offset>`).
cp "$elf" "$root/dist/$name.elf"
# The notices pf-nx-app's build wrote for this link; the NRO's licence (GPL) travels with it.
if [[ -f $root/THIRD-PARTY-NOTICES.txt ]]; then cp "$root/THIRD-PARTY-NOTICES.txt" "$root/dist/"; fi
ls -la "$root/dist/$name.nro" "$root/dist/$name.elf"
