# The console toolchain

What turns this workspace into an NRO for the Switch, beside `scripts/cargo-nx.sh` (the cargo
invocation), `docker/Dockerfile` (devkitA64, libnx, switch-tools and the nightly) and
`scripts/pins.env` (every version).

| Path | What |
| --- | --- |
| `aarch64-nintendo-horizon.json` | The target: `os = "horizon"`, `env = "newlib"`, `rust-lld`, the link script that puts libnx's crt0 first, and `--wrap` for `threadCreate`, `fchmod`, `rename`, `rmdir`. |
| `patches/<crate>/` | The crate legs: crates.io source plus Horizon arms, applied by `.cargo/config.toml`. Each `LEG.md` names the upstream version, the origin and the change. |
| `include/` | C headers devkitA64's newlib lacks (`sys/termios.h` for aws-lc). |
| `skia/` | What `scripts/build-skia.sh` builds Skia with for the console: the `cc`/`c++` wrappers, `args.gn`, `horizon.patch`, `compat.c` (the `mmap` family over the heap), `include/sys/mman.h`, and `layout-check.py`, which proves bindgen's Linux parse of the headers against devkitA64's G++. |

The `--wrap` targets, `userAppInit` (the main thread's TLS pointer) and the entropy symbols newlib
expects are defined in Rust by the binary: `crates/pf-nx-probe/src/shim.rs` today.

## Legs

| Crate | Version | Origin | Why |
| --- | --- | --- | --- |
| `libc` | 0.2.190 | bevy-horizon + punktfunk | devkitA64 newlib's types and layouts; libnx's `msghdr` and message calls |
| `getrandom` | 0.4.3 | bevy-horizon | upstream has no Switch backend (`rand` 0.10) |
| `getrandom` | 0.2.17 | bevy-horizon | upstream takes newlib's `getrandom` on the 3DS only (`rand_core` 0.6) |
| `mio` | 1.2.4 | punktfunk | a `Waker` that wakes a blocked `poll`, for tokio's multi-thread runtime |
| `quinn-udp` | 0.5.16 | punktfunk | the fallback path; the `cmsg` path names options libnx lacks |
| `tokio` | 1.54.1 | punktfunk | a `ucred` arm for Horizon |

A leg applies only while `Cargo.lock` locks its exact version; otherwise cargo warns that the
patch `was not used` and builds crates.io's copy. A bump re-makes the leg from the new release.

## Credit

The target specification and the `libc` and `getrandom` legs are
[bevy-horizon](https://github.com/JohnUzoka/bevy-horizon)'s (John Uzoka, MIT OR Apache-2.0), at
the commit `scripts/pins.env` names. `crates/pf-nx-probe/src/shim.rs` is trimmed from its
`crates/bevy_horizon/src/compat.rs`. Each vendored crate keeps its own licence files.
