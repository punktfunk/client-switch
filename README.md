<div align="center">

<img src="packaging/icon_large.png" alt="Punktfunk" width="130">

<br>
<br>

[![CI](https://github.com/punktfunk/client-switch/actions/workflows/ci.yml/badge.svg)](https://github.com/punktfunk/client-switch/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-d2c9fb)](#licence)

**Native Nintendo Switch client for [Punktfunk](https://git.unom.io/unom/punktfunk) — low-latency desktop & game streaming.**

</div>

---

A homebrew title for the Switch 1 on Atmosphère, built on the upstream `punktfunk-core` crate (a
pinned git dependency — see `Cargo.toml`) and the shared console UI. This repo is only the
Switch-specific client: the libnx platform, NVDEC through libavcodec's `nvtegra` hwaccel, a Vulkan
presenter on Mesa NVK, and the `.nro` packaging.

> **Experimental.** The title and the WP0 probe build, link and package for the console; **nothing
> has run on a console yet.** `docs/plan.md` is the plan of record, `docs/NOTES.md` what is proven
> and how.

## Features

What it is built to do:

- **Video** — 1280×720 handheld and 1920×1080 docked at 60 Hz, H.264 or HEVC on the console's
  NVDEC; the stream follows the dock. No AV1: the Tegra X1 has none.
- **The Punktfunk console UI** — hosts, pairing, game libraries and settings, on Vulkan (Mesa NVK).
  A confirms, B backs, as everywhere on the console.
- **Controllers** — Joy-Cons (attached or paired) and the Pro Controller, up to four players, HD
  rumble, gyro and the touch screen.
- **Audio** — stereo through `audout`.

Not yet: USB keyboard and mouse, PyroWave, LAN discovery on the console, and anything measured —
see `docs/plan.md` §4 for the gates.

## Test it on a console

You need a Switch 1 on Atmosphère, its SD card, and a PC on the same LAN with devkitPro's
`nxlink` (or just the SD card).

1. Copy `dist/pf-nx-probe.nro` and `dist/punktfunk.nro` to `/switch/punktfunk/` on the SD card.
2. Launch the probe from the Homebrew Menu, or push it over the network with
   `nxlink -s dist/pf-nx-probe.nro` while the Homebrew Menu shows the netloader. It prints one line
   per check — `probe N <name> ok|fail …` — on nxlink's terminal or on the screen, and writes the
   same lines to `/switch/punktfunk/probe.log`. Send that file back, with a photo if it crashed.
3. Launch the title the same way, through **title override** (hold R while opening an installed
   game) so it gets the full memory. It logs to `/switch/punktfunk/punktfunk.log`.

Checks 1–3 (std, sockets, QUIC) and the title's menus work in Ryujinx too; check 4 and the stream
need the console (the emulator has no NVK).

## Build it

Everything for the host runs on a Mac or Linux box; the console build runs in a Docker image.
You need [rustup](https://rustup.rs) (`rust-toolchain.toml` installs the pinned nightly),
[Task](https://taskfile.dev) and Docker.

| Command | Does |
| --- | --- |
| `task test` / `task lint` | tests, rustfmt and clippy on this box; the app runs here on `pf-nx-host` |
| `task docker:build` | the toolchain image (devkitPro + the pinned nightly) |
| `task skia` and `task skia -- vulkan` | Skia for the console, once per `skia-safe` version |
| `task nvk` | NVK (`libvulkan_bh.a`), once per Mesa pin |
| `task package` | `dist/punktfunk.nro` |
| `task nvk:probe` | `dist/pf-nx-probe.nro` with all four checks |

The archives live in the `pf-nx-toolchains` Docker volume.

## Repository

| Path | What |
| --- | --- |
| `crates/pf-nx-app` | The app: sessions, the console UI, services, and the platform seams with fakes |
| `crates/pf-nx-platform` | The console's implementation of those seams |
| `crates/pf-nx-host` | The same seams on a developer machine |
| `crates/pf-nx-vk` | Vulkan device, presenter and the NV12 upload |
| `crates/pf-nx-sys` | libnx, libavcodec and NVK by hand, with layout tests |
| `crates/pf-nx-probe` | The probe's checks |
| `toolchain/` | The target, the crate legs and the Skia recipe, from [bevy-horizon](https://github.com/JohnUzoka/bevy-horizon) |
| `scripts/` | Cross build, the archives, packaging |

Read [`AGENTS.md`](AGENTS.md) before changing code: layering, commands and invariants.

## Docs

- [`docs/plan.md`](docs/plan.md): the plan of record.
- [`docs/NOTES.md`](docs/NOTES.md): what is proven, what still needs a console, and the console
  facts the code relies on.
- [`docs/pins.md`](docs/pins.md): every pinned dependency and how it moves.

## Use of AI

This project uses AI for writing code. The rules the agents follow are in [AGENTS.md](AGENTS.md).

## Licence

The source in this repository is MIT OR Apache-2.0, at your option ([`LICENSE-MIT`](LICENSE-MIT),
[`LICENSE-APACHE`](LICENSE-APACHE)).

The built `punktfunk.nro` is GPL-3.0-or-later ([`packaging/licenses/GPL-3.0.txt`](packaging/licenses/GPL-3.0.txt)).
It links devkitPro's `switch-ffmpeg`, whose `nvtegra` hwaccel (the NVDEC driver) is
GPL-2.0-or-later. The Apache-2.0 code combines with GPL version 3 only. The NRO's corresponding
source is this repository at the release's tag, plus FFmpeg 7.1 with devkitPro's patches at
[the pinned recipe](https://github.com/devkitPro/pacman-packages/tree/5cf3a4da7665a04032565a3e788656cb485dd279/switch/ffmpeg).
`THIRD-PARTY-NOTICES.txt` is generated at build time from the dependency graph and ships beside the NRO.
