# punktfunk for Nintendo Switch

A homebrew title for the Switch 1 on Atmosphère that streams games and desktops from a
[punktfunk](https://git.unom.io/unom/punktfunk) host: H.264 and HEVC on the console's NVDEC, the
shared console UI on Vulkan (Mesa NVK), Joy-Cons and Pro Controllers with HD rumble, 1280×720
handheld and 1920×1080 docked at 60 Hz.

**Status:** the title and the WP0 probe build, link and package for the console; **nothing has run
on a console yet.** `docs/plan.md` is the plan of record, `docs/NOTES.md` what is proven and how.

## For the tester

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

## For the developer

Everything for the host runs on a Mac or Linux box; the console build runs in a Docker image.

| Command | Does |
| --- | --- |
| `task test` / `task lint` | tests, rustfmt and clippy on this box; the app runs here on `pf-nx-host` |
| `task docker:build` | the toolchain image (devkitPro + the pinned nightly) |
| `task skia` and `task skia -- vulkan` | Skia for the console, once per `skia-safe` version |
| `task nvk` | NVK (`libvulkan_bh.a`), once per Mesa pin |
| `task package` | `dist/punktfunk.nro` |
| `task nvk:probe` | `dist/pf-nx-probe.nro` with all four checks |

The archives live in the `pf-nx-toolchains` Docker volume. Until the monorepo carries the Horizon
cfgs (`docs/monorepo-horizon.diff`, plan §7), the console build also needs the pinned shared crates
patched into that volume at `/opt/pf-nx/mono`; `scripts/cargo-nx.sh` applies them.

Layout, invariants and the rules for comments and commits: `AGENTS.md`. The crate legs and the
target under `toolchain/` come from [bevy-horizon](https://github.com/JohnUzoka/bevy-horizon), the
NVDEC path is libavcodec's `nvtegra` hwaccel as devkitPro ships it.

## Licence

MIT OR Apache-2.0. `THIRD-PARTY-NOTICES.txt` is generated at build time from the dependency graph.
