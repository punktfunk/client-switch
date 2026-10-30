# Nintendo Switch client — a Rust homebrew title on NVDEC

> **Status: designed 2026-10-28; WP0's probe passed on the console and WP1 closed on 2026-10-29
> (`docs/NOTES.md`, §4). WP2 is next.** Target: the Switch 1 (Erista and Mariko, Tegra
> X1), on Atmosphère, as an `.nro`. The Switch 2 is a later leg behind the same seams (§9); no
> modded one exists to test on. The maintainer's fellow developer holds the only test device, so
> every work package is written for a developer who never holds the console — the PS5 client's
> rule (`punktfunk/client-ps5`, `docs/plan.md`), whose repository shape this copies.
> Decisions D1–D20 are taken (§1); the four the maintainer should confirm first are in §11.

Why native and not Switchroot: a Switch already streams through the Android client on Switchroot
(the 10-24 Joy-Con report). That needs a reboot into another OS. A homebrew title boots from
hbmenu, sleeps and wakes with the console, drives the Joy-Cons' HD rumble and the touch screen
natively, and decodes on NVDEC under Horizon. That is the product; the Android route stays the
answer for anyone who will not install a CFW.

## 0. Briefing for the implementing agent

| Repo | Where | Rules |
|---|---|---|
| `punktfunk/client-switch` (GitHub, new) | the client; sibling checkout `punktfunk-client-switch` | Same workspace rules as `client-ps5`: `unsafe` forbidden in the app crate, `// SAFETY:` everywhere else, the monorepo's `docs/writing.md` for commits, comments and error text. Ask before any push. |
| `unom/punktfunk` (Gitea) | the shared crates, pinned by **one git rev** | §7 lists the monorepo PRs. Target blocks follow the PS5's `cfg(all(target_os = "freebsd", target_vendor = "sie"))` precedent (PRs #1966–#1968). |
| `unom/punktfunk-planning` | this doc | Record each WP's outcome here; move the status line. |
| The console | the fellow developer's, over `nxlink` on his LAN | He runs what the plan hands him: a numbered probe, then builds. Every claim about the console in `docs/NOTES.md` cites a bundle id, as the PS5 repo does. |

- **Read `origin/main` of the monorepo, never a local checkout.**
- **No player-facing setting without the maintainer's yes** (`docs/settings.md`). The shell's
  settings are the console UI's shared catalogue (`clients/shared/settings-catalog.json`).
- **Contract twins are pinned with the shared vectors**, not FFI: `gamepad-kind-vectors.json`,
  `touch-vectors.json`, `device-gyro-vectors.json`, `jitter-vectors.json`.
- **Automatic means automatic**: no bitrate limit, no decoder pick, no clock boost in v1.
- **Prior art is pinned, not forked loosely**: every vendored crate leg names its upstream
  version and the project it came from (§3.1) in its README.

## 1. Decisions

| | Decision |
|---|---|
| D1 | **A separate repository, `punktfunk/client-switch`**, the PS5/webOS/Tizen shape: shared crates by git rev, its own CI, its own release. Not a monorepo member: the target needs a nightly and `-Zbuild-std`, which the workspace must never inherit. |
| D2 | **Switch 1 only** (T210/T214, 4 GB, 1280×720 handheld, 1920×1080 docked, 60 Hz, SDR). The Switch 2 attaches at the seams (§9) when there is a device and a toolchain. |
| D3 | **Rust std through `-Zbuild-std` on the community's Horizon target**, which two shipped projects use today: `bevy-horizon`'s `aarch64-nintendo-horizon.json` (`os = "horizon"`, `env = "newlib"`, `vendor = "nintendo"`, `family = unix`, `rust-lld`, `--wrap=threadCreate` so std threads get working TLS, a link script that puts libnx's crt0 first) with its vendored crate legs (`libc`, `rustix`, `getrandom`, `polling`, `memmap2`; MIT/Apache) and `nightly-2026-10-04`; FlashNX (Ruffle on the Switch) proves the same std on the stock target with two std patches that the `newlib` env makes unnecessary. No std source is patched. `libnx` is linked the way `bevy_horizon`'s build script does: `nx sysbase c m gcc` from devkitA64. Unproven anywhere and therefore WP0: tokio, quinn, aws-lc-rs on this target. |
| D4 | **libnx is the OS**: sockets (`bsd`), threads (newlib pthread through libnx `threadCreate`), `audout`, `hid`, `applet`, `fs`, `nxlink`. No `nx` (pure-Rust Horizon) crate: its std is a to-do and it would be a second OS layer under newlib. |
| D5 | **Graphics: Vulkan on Mesa NVK**, the `mesa-switch` fork both `bevy-horizon` (pin `606441c`) and the Dark Cloud port (`Chronicle` #205, October 2026, "Vulkan 1.3 on the Tegra X1", 60 fps handheld) run on the X1, linked statically as `libvulkan_bh.a`. This makes the Switch the PS5's twin: **`pf-ps5-vk` is lifted** (device, presenter, Skia Vulkan UI layer, PyroWave lane) instead of a GL presenter being written, and `pf-console-ui` takes its Vulkan path. devkitPro's GL is Mesa 20.1 and the Dark Cloud port measured it as the slower path; it is the fallback if NVK fails WP0.4, not a second path. **A second `Display` on libnx's linear framebuffer** (CPU raster, no stream) exists for the probe and the emulator (D15). |
| D6 | **Video decode: NVDEC through libavcodec's `nvtegra` hwaccel** (averne's userspace NVDEC driver; devkitA64 build, enabled by devkitPro's `switchvars`; used by Moonlight-Switch, chiaki, NXMP). H.264 and HEVC. The FFmpeg tree is pinned by rev in `scripts/pins.env` and built `--disable-gpl --disable-everything` plus the two decoders, the hwaccel and the VIC-backed transfer. A Rust NVDEC driver is not written: the hwaccel is months of hardware work already done, and the FFI is one `-sys` module. |
| D7 | **Pictures reach the GPU as pitch-linear NV12** from the hwaccel's VIC transfer, copied from a host-visible staging buffer into the stream image (`vkCmdCopyBufferToImage`), converted by the PS5 presenter's CSC pass with `pf_client_video::video_color`'s rows. One copy per picture (≈3 MB at 1080p) is budgeted for v1; WP5 measures it and imports the nvmap allocation directly only if it shows. |
| D8 | **Audio: `audout`**, 48 kHz stereo S16, one 5 ms Opus frame per appended buffer, three prefilled — the Kodi plan's ALSA shape. Opus through the `opus` crate's libopus, cmake-built for the target as the PS5 does. The console's `hwopus` service is a later lane, not v1. |
| D9 | **Input: libnx `hid`**. Handheld Joy-Cons report as one `JoyConPair`, a Pro Controller as `SwitchPro`, the actual type (maintainer rule). Up to four pads → four player slots. HD rumble from the core's `RumbleCommand` (low/high amplitudes onto the 160/320 Hz pair). Gyro from `hidGetSixAxisSensorStates` onto the device-gyro contract. Touch screen → the touch contract, absolute, as the iPad. USB keyboard and mouse on the dock are §10, not v1. (devkitPro carries an SDL3 branch for Horizon; the gamepad worker could run on it later, but `Pads` on `hid` is 300 lines and exact.) |
| D10 | **Face buttons follow the OS: A confirms, B backs**, in the console UI on this platform. Every Switch app does; a client that swaps them is a bug to the player. (The Android Nintendo-menu question stays the maintainer's; this is the native case of it.) |
| D11 | **Stream mode follows the operation mode**: 1280×720@60 handheld, 1920×1080@60 docked, read at connect and on `AppletHookType_OnOperationMode`; a dock change mid-stream asks the host for the new size (host owns the delivered mode). Refresh is 60. |
| D12 | **Sleep ends the stream** (`OnFocusState`/sleep hook → clean close); wake shows the host list. Reconnect is the player's press. No session survives suspend. |
| D13 | **Full-memory mode first.** The title is validated launched by title override (hold R over an installed title); applet mode (album) is detected (`appletGetAppletType`) and warned about on the start screen, and WP5 measures whether the client fits in it. |
| D14 | **Codecs advertised**: `CODEC_H264 | CODEC_HEVC`, 8-bit, SDR. No AV1 on the X1 (accepted). Main10 only if WP0's probe decodes it through the VIC path. PyroWave is out of v1: the lane lifts with `pf-ps5-vk`, but a 256-core Maxwell at 307–460 MHz has to be measured against the wavelet decode before it is advertised (§10). |
| D15 | **Dev loop without the console**: `pf-nx-host` runs the app on a Linux box through the real presenter on lavapipe and on the Mac on CPU raster, as `pf-ps5-host` does; **Ryujinx (Ryubing) on the Mac runs the real NRO on the framebuffer `Display`** — libnx, sockets, the nvservices the hwaccel talks to are emulated, so everything but the GPU path is proven here. Stock emulators do not run NVK builds (`bevy-horizon`'s finding). Timing from the emulator is never cited. `nxlink -s` is the deploy and the stdout on the device. |
| D16 | **CI**: GitHub Actions like `client-ps5`: a host lane (test, clippy, the host client on lavapipe) and a console lane in a Docker image from `devkitpro/devkita64` plus the nightly, rust-src, and three archives built once per pin (`task nvk`, `task ffmpeg`, `task skia`). The NRO is a CI artifact per PR; releases are tags. |
| D17 | **Logs**: `sdmc:/switch/punktfunk/punktfunk.log` (truncated per launch) and `nxlink` stdout; the console UI's log ring on top. Test-run bundles (log, `environment.json`, a presenter read-back) zipped on the SD card for the tester to fetch, the PS5's `task fetch-bundle` shape. |
| D18 | **Packaging**: `punktfunk.nro` with NACP (name punktfunk, version = the repo's), a 256×256 icon, `romfs` for fonts; installed at `/switch/punktfunk/punktfunk.nro`; GitHub Releases. A Homebrew App Store listing is after v1. |
| D19 | **Licence MIT OR Apache-2.0**; FFmpeg LGPL-only (`--disable-gpl`), Mesa MIT, libnx ISC, Skia BSD, the vendored crate legs under their own licences. `THIRD-PARTY-NOTICES.txt` generated by the app crate's build script as the PS5 does. |
| D20 | **The app layer is lifted from `pf-ps5-app`, not shared.** The project's rule: shell logic stays per platform; the PS5 app was itself lifted from webOS. The `Platform` seams (§2.2) are copied verbatim so the two titles stay structurally identical; consolidating them into one shared crate is a decision for after both consoles are stable (§11). |

## 2. Architecture

```
pf-nx            the console's main: builds a Platform, calls pf_nx_app::run; never returns
pf-nx-app        session, console hosting, services, run loop, the seams + fakes   unsafe forbidden
pf-nx-vk         pf-ps5-vk lifted: device, presenter, Skia Vulkan UI layer, PyroWave lane
pf-nx-sys        hand-written FFI: libnx, libavcodec/nvtegra, the NVK entry; layout tests; shims
pf-nx-platform   the console's seam implementations (pure maps build anywhere)
pf-nx-host       the seams on a dev box: lavapipe on Linux, CPU raster on macOS, SDL pads
pf-nx-probe      WP0's numbered checks, its own NRO on the framebuffer Display
```

Dependencies point down. The app never names a libnx call, a Vulkan handle or an SD path
outside `Storage::data_dir`. The shared crates it takes: `punktfunk-core` (`quic`),
`pf-client-core` (portable + `discovery`), `pf-console-ui`, `pf-client-video` (`video_color`,
`pyrowave` behind a feature).

### 2.1 Threads

| Thread | Does | Priority |
|---|---|---|
| main | the Vulkan device, `Display::frame` at vblank, the console UI, pads | normal |
| tokio (2 workers) | QUIC, FEC, reassembly — `punktfunk-core` | above normal |
| decode | AU in → `avcodec_send_packet`/`receive_frame` → VIC transfer → the free picture slot | high |
| audio | Opus → S16 → `audoutAppendAudioOutBuffer`, blocks on `audoutWaitPlayFinish` | highest |

The X1's four A57s are enough for this; the decode is in hardware. Every thread is a std thread
and so goes through the wrapped `threadCreate` (D3); priorities are libnx's. Threads are never
named from inside themselves (a PS5 trap worth keeping), and `available_parallelism` is
unsupported on Horizon — the tokio worker count is a constant.

### 2.2 The seams (`pf-nx-app::platform`, copied from `pf-ps5-app`)

`Display` (size, ui_size, refresh, `frame(&mut dyn FnMut(&Canvas))`, `set_stream(on, Fit)`,
`read_back`), `VideoDecoders` (`caps`, `open(&StreamSpec) -> Box<dyn VideoSink>`), `AudioOut`,
`Pads`, `Storage`, `Lifecycle`, `DeviceInfo`. Two additions for this console: `Display::mode()`
returns the operation mode (handheld/docked) and `Lifecycle` carries the applet hooks (operation
mode, focus, sleep). The contract tests in `tests/seams.rs` are the spec; `fake` implements all.

## 3. The three hard problems

### 3.1 The toolchain: assembled, then proven for QUIC

What exists (2026-10-28):

| Piece | Source | State |
|---|---|---|
| Target JSON, link script, `--wrap`s | `JohnUzoka/bevy-horizon` `aarch64-nintendo-horizon.json` | runs Bevy 0.18/0.19 on a Switch Lite; threads, files, sockets, audio, HOME, sleep pass its `switch_captest` |
| `libc` 0.2.190 leg | bevy-horizon `patches/` (devkitA64 newlib types, layouts, constants) | applied through `.cargo/config.toml`, because std builds as its own workspace |
| `rustix` 1.1.5 leg | bevy-horizon (devkitA64 types differ from the 3DS upstream) | |
| `getrandom` 0.4 + 0.2 legs | bevy-horizon (`arc4random_buf` over `randomGet`; 0.2 for ring) | |
| `ring` 0.17 leg | bevy-horizon (aarch64 asm, `linux64` perlasm flavour) | the fallback crypto provider (§8) |
| std on the stock target | `Jonathan8520/FlashNX` (Ruffle) | two std patches the `newlib` env replaces; `read_dir` buggy, a `std::thread` spawn crashed its JPEG decoder — bevy-horizon's `--wrap=threadCreate` is the fix for the second |
| libnx link set | `bevy_horizon/build.rs`: `nx sysbase c m gcc` + devkitA64's `pic` libgcc | devkitA64 r30-1, libnx 4.12.0 |
| NRO tooling | devkitPro `elf2nro`, `nxlink`; `cargo-nx` for the netloader | |

What no one has run on the console: **tokio, quinn, aws-lc-rs**. tokio needs mio's selector on
Horizon (the Vita's `poll(2)` path is the precedent; a leg if missing); quinn-udp's `sendmsg`
path needs libnx's `bsd` to carry it (else its fallback, behind a leg); aws-lc-rs must
cross-build under devkitA64's GCC 16 with the PS5's `getentropy` and fork-detection shims, or
the `ring` leg carries the provider for this target. `if-addrs` is excluded for Horizon as for
wasm (§7). This is WP0.1–0.3, one week, and the only place the plan can still stop: if QUIC
cannot stand on this target, nothing in punktfunk can run on the console, in any language.

### 3.2 NVDEC from Rust

The hwaccel is C inside libavcodec. `pf-nx-sys::av` declares the dozen functions the decode
thread uses (`avcodec_find_decoder_by_name`, `av_hwdevice_ctx_create` for `nvtegra`,
`send_packet`/`receive_frame`, `av_hwframe_transfer_data`, the buffer/frame unrefs) by hand —
no `ffmpeg-sys` crate, which would bindgen the whole tree for a target bindgen must be taught.
The decode lane is the PS5's `Videodec2` lane with the FFmpeg calls in place of the console's,
ending in the same staging-buffer copy into the presenter's image. What to measure before
building on it (WP0.6): frames in flight (the host's one-slice-per-frame default means the
decoder should hold at most one), decode time per 1080p60 picture, the transfer's cost, whether
10-bit survives the VIC path.

### 3.3 NVK and Skia for `aarch64-none-elf`

NVK: `scripts/build-nvk.sh` is bevy-horizon's `build_nvk.sh` + `localize_nvk.sh` at the same
`mesa-switch` pin, producing `libvulkan_bh.a` (+ `expat zstd z stdc++`); `pf-nx-sys` links it
and hands `ash` the static entry point, as `pf-ps5-vk` does with RADV. Known: a lit Bevy scene
hung the GPU after ten seconds, cause unknown; the Dark Cloud port runs 60 fps on it and sets the
GPU clock to the official 460.8 MHz handheld configuration (not 307.2) — WP0.4 reads what clock
the title gets. NVK is not conformant on the X1; the presenter uses the PS5's conservative
feature set.

Skia: the PS5's `scripts/build-skia.sh` already builds the ganesh-textlayout-vulkan archive for a
foreign target; the Switch variant points it at devkitA64's sysroot and serves it through
`SKIA_BINARIES_URL`. If the first week does not produce an archive, the fallback is the console
UI on CPU raster uploaded as one image a frame — webOS shipped exactly that at 1080p on armv7.

## 4. Work packages and exit criteria

### WP0 — Toolchain and runtime (one week, timeboxed)

`pf-nx-probe` is an NRO of numbered checks on the framebuffer `Display`; each prints one
`probe N <name> ok|fail <detail>` line over nxlink and into the bundle. The developer runs it in
Ryujinx; the tester runs it on the console. In order:

1. `std`: hello over nxlink, four threads with `thread_local!`, `HashMap`, `Instant`/`SystemTime`,
   a file on `sdmc:`, `read_dir`, a panic that writes the log before `abort`.
2. `sockets`: UDP bind/send/recv, `sendmsg`/`recvmsg`, `SO_RCVBUF` growth to 4 MiB,
   `IP_ADD_MEMBERSHIP`, `getaddrinfo`; `socketInitialize` with the UDP buffers sized for 4K
   bursts (`SocketInitConfig::udp_rx_buf_size`).
3. `quic`: `punktfunk-core` with `quic` dials a host, pairs with a PIN, and decrypts one media
   packet; tokio on two workers; which crypto provider linked.
4. `vulkan`: NVK instance, device, swapchain at the panel's mode, clear + one textured quad,
   600 frames with the frame-time histogram; the GPU clock seen. (On the console only.)
5. `skia`: the console's Hosts screen through `pf-console-ui` on Vulkan, 60 fps, or the raster
   fallback's fps.
6. `nvdec`: a 1080p60 H.264 and an HEVC file from `sdmc:` through `nvtegra`: fps, decode time
   per picture, frames in flight, the VIC transfer's cost, first picture latency, 10-bit.
7. `audio`: `audout` 48 kHz stereo, 5 ms buffers, 60 s underrun-free, the output latency.
8. `hid`: the pad kinds seen, HD rumble, gyro rates, touch; the operation mode and its hook.
9. `memory`: free heap in title-override and in applet mode.

**Exit:** 1–4 and 6 green on the console → go. 5 may take the fallback. Everything measured
goes into `docs/NOTES.md` under "Proven on the console" with the bundle id.

### WP1 — The repository and the app on a dev box (one week)

The layout of §6, the Taskfile, `rust-toolchain.toml`, the target JSON and the crate legs under
`toolchain/`, `scripts/cargo-nx.sh` (build-std, the cc/cmake/bindgen exports for the target),
the Docker image, CI host lane. `pf-nx-app` lifted from `pf-ps5-app`: the seams, fakes and
`tests/seams.rs` verbatim; session, services, runtime, logger, ui with the PS5-only parts cut
(DualSense triggers, touchpad chords, Videodec2 plane sets, 4K120). `pf-nx-vk` lifted from
`pf-ps5-vk` with RADV's entry swapped for NVK's. `pf-nx-host`: lavapipe on Linux, raster on
the Mac, the software rung, SDL pads.

**Exit:** `task test` green on the Mac; the host client lists hosts, pairs and streams a game
on a Linux box through the real presenter; `task cross-check` type-checks the console code.

**Outcome (2026-10-29).** The dev box is a container (`docker/host.Dockerfile`, `task
docker:host-test`) on the maintainer's Windows PC, which also runs the host; no Mac or Linux box
took part. There `task test` is green, and `pf-nx-host` lists the PC's host, pairs from a test
run's PIN and streams 1080p60 HEVC through the presenter on lavapipe, decoded by the console's
own video lane on libavcodec's software decoder: 770 pictures in 20 s, decode to present p50
12 ms, none dropped. The library loads with its covers. `task cross-check` type-checks the
title and the probe; CI's console lane builds `punktfunk.nro` beside the probe. Departures:

- The pads are scripted, not SDL: a container on Windows sees no controller, and the developer
  who tests the dev box is the agent. `test-run.json`'s `pads` presses through the menu and
  saves a frame per step. SDL pads wait for a Linux or Mac box that wants them.
- The UI rasters on the CPU there; `skia-vulkan` is the console's.
- mDNS does not cross Docker's network: a test run names the host and its management port.

### WP2 — The console on the console (two weeks)

`pf-nx-sys` (libnx: applet, hid, audout, fs, socket init; the NVK entry), `pf-nx-platform`
(`Display` on NVK and on the framebuffer, `Storage` on `sdmc:`, `Lifecycle` with the hooks,
`Pads`, `DeviceInfo` from `setsysGetFirmwareVersion`). Discovery over the LAN, pairing, the
settings screens, the log ring, the test-run bundle.

**Exit (G2):** the NRO boots to the Hosts screen in Ryujinx (framebuffer) and on the console
(NVK); a host is found, paired, named; the UI holds 60 fps on glass; A confirms; the bundle
fetches.

**Outcome (2026-10-30).** On the tester's Switch 1 the title boots on NVK to the Hosts screen,
finds the PC's host over mDNS, pairs by PIN, renames it, streams 720p60 HEVC through NVDEC and
quits back to hbmenu; A confirms, B backs, the settings persist, and the test run writes its
bundle to `sdmc:/switch/punktfunk/verdict/` (`docs/NOTES.md`, title at `927533b`). Departures:

- No Ryujinx run: the tester is the developer and holds the console, so the emulator's reason
  is gone. Ryubing recognised the NRO and then closed itself, untraced; the framebuffer
  fallback waits for a console where NVK fails.
- The menu holds 60 fps in most ten-second windows, not all: some lose up to 33 frames to
  single frames of 65–213 ms. WP5 traces them.
- The bundle is a folder of PNGs and JSON on the SD card, not a zip, fetched by hand.

### WP3 — The video plane (three weeks)

The decode lane (`pf-nx-platform::video` over `pf-nx-sys::av`), the stream image path in
`pf-nx-vk` (staging copy, the CSC pass, `Fit`), present pacing to vblank, the stats HUD,
recovery (decoder error or stall → `request_keyframe`, the session's `recovery_on_*` pattern),
the dock change mid-stream (D11), `set_client_label("Switch")` so the console's device list
names it.

**Exit (G3):** first picture on glass; 1080p60 docked and 720p60 handheld for ten minutes each,
`stats:` clean, decode-to-present under one frame, the dock change re-sizes without a redial.

### WP4 — The first playable client (two weeks)

Audio out with the jitter tuning, pads with rumble and gyro, touch, the overlay menu and the
end-stream chord, sleep handling (D12), applet-mode warning (D13). This is the build the fellow
developer gets as "the initial version".

**Exit (G4):** an hour of a game on glass with pad, rumble and audio in sync; a second pad joins;
sleep and wake return to Hosts; the tester's report answered.

### WP5 — Reliability and latency (two weeks)

Measured, not guessed: the picture copy (D7) and nvmap import if it shows; the audio sync cell
if the presentation clock drifts from it; Wi-Fi loss behaviour against the host's recovery;
memory in applet mode; the GPU clock the title gets and whether the official handheld
configuration is reachable without an opt-in; PyroWave on NVK against the GPU budget (D14).
Ten-hour soak.

### WP6 — Package and release (one week)

NACP, icon, romfs, `THIRD-PARTY-NOTICES.txt`, `docs/tester.md` as the manual, the release
workflow on tags, the monorepo's download page row.

## 5. Developing without the console

- **Host platform** on the Mac and Linux: everything above the seams, every day; the real
  presenter and Skia-on-Vulkan on lavapipe.
- **Ryujinx (Ryubing)**: the real NRO on the framebuffer `Display`. libnx, `bsd`, `nvhost-nvdec`,
  `nvmap` and the VIC are emulated, so the hwaccel's code path runs; H.264 decode is emulated
  through FFmpeg, HEVC is to be checked in WP0. NVK builds do not run there. Timing from the
  emulator is never cited.
- **nxlink**: `nxlink -s punktfunk.nro` deploys and streams stdout; the tester runs the same.
- **Bundles**: a test run writes `sdmc:/switch/punktfunk/bundles/<id>.zip`; the tester copies it
  from the SD card or over FTP (sys-ftpd). Every console claim cites one.
- **Rules**: no workaround without a probe line; a console fact lives in `docs/NOTES.md`.

## 6. Repository layout

```
client-switch/
  Cargo.toml                    workspace; shared crates by one rev; lints as client-ps5
  rust-toolchain.toml           nightly-2026-10-04; rust-src
  toolchain/aarch64-nintendo-horizon.json      the target (bevy-horizon's, pinned, credited)
  toolchain/patches/{libc,rustix,getrandom,…}  the crate legs, each README: upstream + origin
  toolchain/nx.cmake, toolchain/bin/cc …       toolchain file + cc wrappers for the target
  scripts/pins.env              devkitPro packages, mesa-switch, FFmpeg, skia-safe, PUNKTFUNK_REV
  scripts/cargo-nx.sh, build-nvk.sh, build-ffmpeg.sh, build-skia.sh, package-nro.sh
  Taskfile.yml, taskfiles/docker.yml
  docker/Dockerfile             devkitpro/devkita64 + nightly + the three archives
  src/main.rs                   pf-nx
  crates/pf-nx-{app,vk,sys,platform,host,probe}
  packaging/icon.jpg, nacp.toml, romfs/
  docs/plan.md (this), docs/NOTES.md, docs/tester.md, docs/pins.md
  .github/workflows/ci.yml, release.yml
```

## 7. Monorepo PRs (each its own, `Part of` the client)

1. `feat(client): build the client crates for Horizon` — the PS5's target blocks
   (`pf-client-core` portable + `discovery`, `pf-console-ui`, `pf-client-video`) extended with
   `all(target_os = "horizon", target_arch = "aarch64")`.
2. `fix(core): skip if-addrs on Horizon` — as wasm already does — plus whatever WP0.2 finds the
   `bsd` service lacks (QoS setsockopts behind a cfg). A crypto-provider cfg only if WP0.3 ends
   on `ring`.
3. `feat(core): name the Switch in the client label` — `Platform::Switch`, as `Platform::Ps5`.
4. If WP0.8 shows a pad the kind table does not name, the vectors in
   `clients/shared/gamepad-kind-vectors.json` grow with it (Android's `JoyConPair` precedent).

## 8. Risks

| Risk | Signal | Answer |
|---|---|---|
| tokio/quinn do not stand on Horizon | WP0.3 not green in the week | stop; re-cut (§3.1) |
| aws-lc-rs will not cross-build with GCC/newlib | WP0.3 link errors | the PS5's shims; else the `ring` leg as this target's provider, behind a cfg in core |
| NVK hangs or is slower than the panel | WP0.4 histogram, the bevy hang | the GL fallback (Mesa 20.1, Skia `gl`) — the plan's original shape, one presenter rewrite |
| Skia archive | §3.3 | raster fallback |
| `nvtegra` is not in devkitPro's `switch-ffmpeg` at the pinned version | WP0.6 | build the pinned fork; `scripts/build-ffmpeg.sh` exists for it either way |
| The crate legs drift | every nightly or pin bump | `scripts/pins.env` pins them; a bump is a PR with the probe re-run |
| Applet mode out of memory | WP0.9, WP5 | title-override documented as required; applet mode best effort |
| The emulator hides a device-only fault | every round | the tester's bundle decides; Ryujinx is never cited |
| One device, one tester | throughput | rounds are batched; the probe lists what to run |

## 9. Switch 2

Not verifiable: no device, and whether a homebrew toolchain for the T239 exists in the open
cannot be established from here. What the design keeps ready: `VideoDecoders::caps()` is the
only place the decodable codecs live (AV1 and 10-bit would appear there), `Display::size`/
`refresh`/`mode` are the only places the panel's mode lives (1080p120 handheld, 4K docked, HDR
would appear there), and nothing above the seams assumes 720p, 1080p or 60 Hz. A Switch 2 leg
is a second `pf-nx-platform` cfg, not a fork.

## 10. After the baseline

PyroWave on NVK if WP5's budget allows; nvmap import for the stream image (if WP5 leaves it);
USB keyboard and mouse on the dock (`hidKeyboard*`/`hidMouse*` → the keymap contract);
`hwopus`; the GPU-clock question if WP5 shows a case; the Homebrew App Store listing; the
Switch 2.

## 11. Open before WP0 starts (the maintainer's)

1. **The repository**: `punktfunk/client-switch` on GitHub, public from the first commit as
   `client-ps5` is — fine?
2. **D10, A confirms**: the native case of the open Android Nintendo-menu question. Yes here?
3. **D20, lift versus share**: lift the PS5 app layer now; the shared `pf-console-app` question
   returns when both consoles are stable. Or share from the start and couple the two.
4. **The timebox**: WP0 one week, v1 (WP0–WP4) about nine weeks with the tester's rounds inside,
   WP5–WP6 three more.
