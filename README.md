<div align="center">

<img src="packaging/icon_large.png" alt="punktfunk" width="130">

# punktfunk for Nintendo Switch

**Play your PC games on your Switch, streamed over your home network.**

[![Latest release](https://img.shields.io/github/v/release/punktfunk/client-switch?label=download)](https://github.com/punktfunk/client-switch/releases/latest)
[![CI](https://github.com/punktfunk/client-switch/actions/workflows/ci.yml/badge.svg)](https://github.com/punktfunk/client-switch/actions/workflows/ci.yml)

</div>

---

punktfunk runs your games on your PC and sends the picture and sound to your Switch. It is a native homebrew app: it opens from the Homebrew
Menu (no applet mode), uses the Switch's own video decoder, and works handheld or docked. Handheld has been more tested and it's the recommended mode to play.

## What you need

- A **Nintendo Switch 1** (original, Lite or OLED) running homebrew on **Atmosphère**.
- A PC with the **[punktfunk host](https://docs.punktfunk.unom.io/docs/install)** installed, on
  the same network.
- A good connection: 5 GHz Wi-Fi near the router, or a wired dock. A wired PC helps too.

## Install

1. Download **`punktfunk.nro`** from the [latest release](https://github.com/punktfunk/client-switch/releases/latest).
2. Copy it to **`/switch/punktfunk/`** on your SD card.
3. **Hold R while opening any game**, then pick punktfunk in the Homebrew Menu. Run from Sphaira is recommended. Applet mode does not work. 

> **Don't open it from the Album.** The Album gives homebrew too little memory, and punktfunk
> crashes there.

Then pick your PC on the **Hosts** screen, enter the PIN the host shows, and choose a game. 
You can check more in punktfunk [quickstart](https://docs.punktfunk.unom.io/docs/quickstart) documentation.

## What it does

- **720p at 60 fps handheld, 1080p at 60 fps docked.** Docking mid-game switches without
  reconnecting is still being working. 
- **Your controllers, as they are.** Joy-Cons and the Pro Controller, with HD rumble, gyro and
  the touch screen.
- **Low latency.** About 30–45 ms from the PC's screen to yours, on a good network.
- **Two presentation modes.** *Smoothness* gives a very smooth picture at an acceptable
  latency. *Low latency* cuts the delay further, at the cost of some smoothness.
- **Easy to set up, easy to use.** You pair once, and from then on it is one
  click to connect. The menus are built for a controller and a touch screen, so you never
  need a keyboard.
- **A modern, encrypted protocol.** The stream runs on punktfunk's own protocol, built on
  QUIC and encrypted end to end. It is designed for low latency and recovers from lost
  packets without freezing the picture.

| In a stream | Does |
| --- | --- |
| **Minus + A** | Quick actions: statistics, controller mouse, controller type |
| Hold **L + R + Plus + Minus** for one second | End the stream |

## Known limits

- Docked play has been tested less than handheld.
- Overlocking is necessary to better performance and latency. At stock clocks it works, but raising the CPU clock makes menus smoother and lowers the stream's latency.
- No launching from the Album.
- No USB keyboard or mouse yet.
- Switch 2 is not supported yet.

## Found a problem?

Open an [issue](https://github.com/punktfunk/client-switch/issues) and attach the log from
`/switch/punktfunk/logs/` on your SD card. The [manual](docs/tester.md#reporting-a-problem)
says which file to send.

---

## For developers

A Rust title on libnx, with NVDEC through libavcodec's `nvtegra` hwaccel and a Vulkan presenter
on Mesa NVK, built on the shared [punktfunk](https://git.unom.io/unom/punktfunk) crates.
[`docs/plan.md`](docs/plan.md) is the plan of record and [`docs/NOTES.md`](docs/NOTES.md) what is
proven on the console. Every PR's CI uploads `punktfunk.nro` and the WP0 probe
(`pf-nx-probe.nro`).

### Build it

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

### Repository

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

### Docs

- [`docs/plan.md`](docs/plan.md): the plan of record.
- [`docs/NOTES.md`](docs/NOTES.md): what is proven, what still needs a console, and the console
  facts the code relies on.
- [`docs/pins.md`](docs/pins.md): every pinned dependency and how it moves.

### Use of AI

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
