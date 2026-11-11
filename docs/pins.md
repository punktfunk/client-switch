# Pins

`scripts/pins.env` is the source; this says what each pin is for and how it moves.

| Pin | Value | What it fixes | Moves when |
| --- | --- | --- | --- |
| Rust nightly | `nightly-2026-10-04` (rustc 1.101.0, `db8f076d2`) | `-Z build-std` for the custom target; the legs were made against it | a std change is needed; re-run the probe |
| punktfunk | `9eb2eb641` | `punktfunk-core`, `pf-client-core`, `pf-console-ui`, `pf-client-video`, `pf-l10n`, one rev for all | the shared crates change; one bump, all five |
| devkitA64 | `r30-1` (GCC 16.1.0, newlib 4.6.0) | the C compiler, newlib, libgcc's `pic` objects | devkitPro ships a new one: `-Syu` in the image fails until this moves |
| libnx | `4.12.0-1` | crt0, sockets, threads, the services | with devkitA64 |
| switch-tools | `1.13.1-1` | `elf2nro`, `nacptool`, `nxlink` | with devkitA64 |
| switch-ffmpeg | `7.1-5`, recipe `5cf3a4da7` | libavcodec with the `nvtegra` hwaccel. GPL-2.0-or-later, so the NRO is GPL (plan D19); the recipe in `devkitPro/pacman-packages` at that commit is its corresponding source | devkitPro rebuilds it: move the recipe commit with the version |
| switch-dav1d | `1.5.0-1` | the AV1 decoder libavcodec's registry links | with switch-ffmpeg |
| devkitpro/devkita64 | `sha256:1fc388c3…` | the image's base (amd64 + arm64), before `-Syu` | with devkitA64 |
| bevy-horizon | `3eac779b2` | the target JSON, the `libc` and `getrandom` legs, the shims | a fix lands there; diff its `patches/` and target first |

A bump changes `scripts/pins.env` (and `rust-toolchain.toml` / `Cargo.toml` / `docker/Dockerfile`
for the ones they carry), bumps `NX_IMAGE`, and re-runs the probe on the console before anything
else ships. The crate legs' upstream versions are in `toolchain/README.md`.

## Skia

`scripts/build-skia.sh` reads these from the locked skia-bindings crate, so nothing here is typed
twice. A skia-safe bump moves all three; `toolchain/skia/horizon.patch` is re-made on the new tree.

| What | Value | From | In the archive's name |
| --- | --- | --- | --- |
| skia-safe / skia-bindings | `0.153.3` | `Cargo.lock` | `{tag}` |
| skia-bindings hash | `b7f043e0b1e2a850e702` | the crate's `.cargo_vcs_info.json` | head of `{key}` |
| Skia | `m153-0.101.2` (rust-skia's fork) | the crate's `[package.metadata] skia`; its `DEPS` pins third_party and gn | — |

The archive is `/opt/pf-nx/skia/{tag}/skia-binaries-{key}.tar.gz`, `{key}` being the hash, the
target, then the sorted features: `aarch64-nintendo-horizon-jpegd-jpege-pdf-textlayout` for CPU
raster, `aarch64-nintendo-horizon-ganesh-jpegd-jpege-pdf-textlayout-vulkan` for `skia-vulkan`.
The volume holds an arm64 host's build: raster 19 969 272 bytes, sha256 `7c37cd2403ba196b…`;
vulkan 22 748 757 bytes, sha256 `6a670e066df15b39…`. The tar carries mtimes, so a rebuild's differ.

## NVK

`scripts/build-nvk.sh` (`task nvk`) builds `libvulkan_bh.a` into the archives volume at
`/opt/pf-nx/nvk/lib`: bevy-horizon's `build_nvk.sh` and `localize_nvk.sh` at `BEVY_HORIZON_REV`,
in bevy-horizon's toolchain image built on `NX_IMAGE`, so it never calls devkitPro's pacman.

| What | Value |
| --- | --- |
| mesa-switch | `606441c90e459028a50fca352daebfc20211f890` (Mesa 26.2.4, danfromtico/mesa-switch); `MESA_SWITCH_REV` in the script |
| Patches | bevy-horizon's `patches/mesa-switch/`, in order (below) |
| Built by | devkitA64 r30-1, libnx 4.12.0-1, NAK on rustc 1.101.0-nightly `db8f076d2`, meson 1.12.1; arm64 host |
| `libvulkan_bh.a` | 45 433 534 bytes; its one member `vulkan_bh.o` sha256 `55a6eb01931fdaea478813584319f7ba0748f69258a185a8fb41363076bc35fc` |
| Global symbols | 768 `vk*` entry points, `nouveau_horizon_region_{alloc,free,stats}`, `nouveau_horizon_tune` |

The patches: 0001-top-down-gpu-backing, 0002-nvk-switch-timestamp-period, 0003-nak-debug-stats,
0004-nak-cross-block-barriers, 0005-nak-sink-remat, 0006-nak-uniform-branches, 0007-nak-xbrk,
0008-nvk-switch-tune, 0009-nvk-switch-zcull-direction, 0010-nvk-switch-zcull-report,
0011-nak-loop-sink, 0012-nvk-switch-skip-empty-draws, 0013-nvk-switch-compression-report,
0014-nak-gpr-limit, 0015-nak-prepass-latency, 0016-nak-tests-build,
0017-nvk-compute-invocation-queries, 0018-mme-fermi-mul-clear, 0019-nak-switch-defaults,
0020-nvk-deferred-cs-invocations, 0021-nak-ofold, 0022-nak-wgbar, 0023-nak-share, 0024-nak-jthr.

It moves with the bevy-horizon pin or mesa-switch: rerun `task nvk` and update the size and hash
here. The archive's own hash changes every build (`ar` stamps the time), so compare the member's.
The hash is an arm64 host's; an amd64 build is not checked against it.
`bevy-horizon-nvk:local` is reused when present: remove it when the base image moves.
