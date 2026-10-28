# Pins

`scripts/pins.env` is the source; this says what each pin is for and how it moves.

| Pin | Value | What it fixes | Moves when |
| --- | --- | --- | --- |
| Rust nightly | `nightly-2026-10-04` (rustc 1.101.0, `db8f076d2`) | `-Z build-std` for the custom target; the legs were made against it | a std change is needed; re-run the probe |
| punktfunk | `e7da9ad3d` | `punktfunk-core`, `pf-client-core`, `pf-console-ui`, `pf-client-video`, one rev for all | the shared crates change; one bump, all four |
| devkitA64 | `r30-1` (GCC 16.1.0, newlib 4.6.0) | the C compiler, newlib, libgcc's `pic` objects | devkitPro ships a new one: `-Syu` in the image fails until this moves |
| libnx | `4.12.0-1` | crt0, sockets, threads, the services | with devkitA64 |
| switch-tools | `1.13.1-1` | `elf2nro`, `nacptool`, `nxlink` | with devkitA64 |
| devkitpro/devkita64 | `sha256:1fc388c3…` | the image's base (amd64 + arm64), before `-Syu` | with devkitA64 |
| bevy-horizon | `3eac779b2` | the target JSON, the `libc` and `getrandom` legs, the shims | a fix lands there; diff its `patches/` and target first |

A bump changes `scripts/pins.env` (and `rust-toolchain.toml` / `Cargo.toml` / `docker/Dockerfile`
for the ones they carry), bumps `NX_IMAGE`, and re-runs the probe on the console before anything
else ships. The crate legs' upstream versions are in `toolchain/README.md`.
