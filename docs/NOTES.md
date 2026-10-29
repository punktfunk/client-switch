# Console notes

Facts about the console and what is proven, each citing a bundle id. Nothing yet.

## Toolchain

What builds, and how; nothing here has run on a console.

- `scripts/docker.sh` runs a command in the image on a copy of the tree in the `pf-nx-src`
  volume (no bind mount); `target/` and cargo's registry live in volumes, `dist/` comes back out.
- `scripts/cargo-nx.sh check|build -p pf-nx-probe` cross-builds; `scripts/package-nro.sh
  pf-nx-probe` writes `dist/pf-nx-probe.nro`. Checks 1–2 build with `--no-default-features`.
- Check 3 (`quic`) needs punktfunk-core's Horizon arm for `if-addrs` (monorepo #2180, in the pin).
- aws-lc builds with its CC builder: no BIO sockets, CPU features from `-march`, getentropy for
  entropy and no CPU jitter entropy (`scripts/cargo-nx.sh`). rustls's provider is aws-lc-rs; ring is not linked.
- quinn-udp runs its fallback (no ECN, one datagram per call) until the console shows `sendmsg`.
- tokio's I/O driver wakes through a loopback UDP socket (the mio leg): unproven on the console.
- devkitPro's `switch-ffmpeg` in the image carries the `nvtegra` hwaccel (`libavutil/
  hwcontext_nvtegra.h`; the symbols are in `libavcodec.a`), so plan D6 needs no FFmpeg fork.
  The portlibs also hold `libopus.a`, `libEGL.a` and `libdrm_nouveau.a`.
- `pf-nx-sys` cross-clippies clean for the console without features; with `av` the C shim
  compiles against the image's libavcodec headers.
- The whole title type-checks and links for the console: `scripts/cargo-nx.sh build -p pf-nx`
  then `scripts/package-nro.sh pf-nx punktfunk` writes `dist/punktfunk.nro` (78 MB: the image's
  full libavcodec with its dav1d and swresample closure, Skia, NVK, line tables).
- `scripts/build-nvk.sh` builds NVK cold, both images included, in about four minutes on the Mac
  (arm64, `JOBS=12`); a rerun reuses the tree and the patched checkout. The bevy-horizon
  Containerfile names rustup's x86_64 directory; the script swaps in the host's.
- The probe links NVK with `--features vulkan`: a 15.8 MB NRO whose relocations are all
  `R_AARCH64_RELATIVE`. The only undefined symbols left are the dispatch tables' weak driver
  entry points, which resolve to null. NAK's and NIL's Linux std references (`fork`, `dlsym`,
  `posix_spawnp`) sit in sections the link discards.
- `nouveau_horizon_region_{alloc,free,stats}` are defined in the archive, not needed from the
  title: unused, the link drops them. NVK still takes GPU backing from the top of the heap
  (patch 0001; `NOUVEAU_HORIZON_TOP_DOWN=false` turns it off).
- Ryujinx does not run NVK builds (plan D15): check 4 runs on the console only.
- `scripts/build-skia.sh [raster|vulkan]` (`task skia`) compiles Skia with devkitA64's G++
  through skia-bindings' offline build and writes the archive cargo-nx.sh's `SKIA_BINARIES_URL`
  names (docs/pins.md). `check -p skia-safe` takes the `vulkan` one: a `-p` outside the workspace
  unifies pf-nx-platform's `skia-vulkan` in. `check -p pf-nx-app` takes `raster`.
- devkitA64 defines no `__unix__`, so Skia took its Mac branch; `toolchain/skia/horizon.patch`
  names the platform in gn, where skia-bindings reads its defines. It also gives the console
  Skia's stdio log port, and zlib its portable C path: zlib's gn asserts on an unknown ARM OS.
- Under `-std=c++20` newlib hides POSIX (`fileno`): the compiler wrappers add `_GNU_SOURCE`, as
  g++ does on Linux. newlib has no `sys/mman.h`: Skia and ICU get a header, and `libskia.a` gets
  weak `mmap` (a heap copy), `munmap`, `pread` and `posix_madvise` (`toolchain/skia/compat.c`).
- bindgen parses as aarch64 Linux: clang 14, and libclang 18 too, reject libstdc++ 16's
  `__normal_iterator`. `toolchain/skia/layout-check.py` holds G++'s sizes and alignments to
  bindings.rs on every build: none differ (raster 352 of 383 types, vulkan 390 of 426; the rest
  are private or anonymous).
- Either archive leaves one symbol to the title, `sysconf` (pf-nx-sys has it), and needs
  `-lstdc++`, which pf-nx-sys links only with `nvk`. `kN32` is RGBA: Linux's BGRA comes from
  `is_linux` alone. `FontMgr::new()` is the empty manager; the title's fonts load from data.

## Proven on the console

Runs on the tester's Switch 1 (Atmosphère, from hbmenu). No bundles yet: each item cites the
probe build's commit and the run's date.

- Check 1 (`std`) passes: four threads with their own `thread_local!`, `HashMap`, a 10 ms sleep,
  the wall clock, a file on `sdmc:` and `read_dir` (probe at `5222fb9`, 2026-10-28).
- `socketInitialize` takes 4 MiB UDP buffers, but every UDP socket then fails with ENOBUFS
  (`os error 105`); `getaddrinfo` still answers (probe at `5222fb9`, 2026-10-28). The UDP sizes
  are each new socket's reservation, so the probe starts sockets on libnx's defaults and check 2
  measures how far `SO_RCVBUF` grows instead.
- On libnx's default UDP sizes (42240 rx, 9216 tx) UDP send and receive, `sendmsg`/`recvmsg`, the
  mDNS group join and `getaddrinfo` all work. `SO_RCVBUF` takes 1 MiB at most: 4 MiB and 2 MiB
  fail with ENOBUFS, and `getsockopt` reads 1048576 back (probe at `4b0bd19`, 2026-10-28).
- With check 3 linked, checks 1 and 2 read as before, then the probe dies in check 3 before its
  line: a data abort reading address 0 on the main thread, with tokio's two workers already up.
  Resolved against `2e66719`'s ELF (the same code but for check 3's log lines), the abort is
  `mrs x0, cntvct_el0` in aws-lc's `jent_get_nstime`, under `EVP_PKEY_keygen`'s first random
  bytes: Horizon does not let EL0 read the virtual counter (libnx reads CNTPCT_EL0). The console
  build sets `AWS_LC_SYS_NO_JITTER_ENTROPY`, so aws-lc seeds from getentropy (probe at `528c0ee`,
  2026-10-28; Atmosphère report `01793228241_05446530aca7e000`).
- Without jitter entropy, check 3 passes: tokio on two workers, an EC identity through aws-lc-rs
  (542-byte certificate) and punktfunk-core's client endpoint bound on the shared UDP socket. No
  host was dialled yet (probe at `d90d956`, 2026-10-28).
- Check 3 against a real host on the LAN (Wi-Fi): PIN pairing, then a session at 720p60 that
  negotiated HEVC (`codec=2`), and the first video frame received, reassembled and decrypted
  3061 ms after the connect returned. The pairing was kept on the SD card and reused on the next
  run (probe at `3a07655`, 2026-10-28).
- Check 4 (`vulkan`) passes with NVK linked: loader 1.4.354 with `VK_NN_vi_surface` among 15
  instance extensions, no layers, one GPU `NVIDIA Tegra X1 (GM20B) (NVK GM20B)` (10de:0fe0,
  driver 26.2.4) with one graphics/compute/transfer queue and a 2391 MiB heap, a device on
  family 0, and a 16 MiB host-visible buffer written in 1.8 ms and read back in 38.8 ms (probe
  at `f0b53c0`, 2026-10-29).
- In the same run check 3 paired from the saved fingerprint and negotiated HEVC, but no frame
  came in 15 s (probe at `f0b53c0`, 2026-10-29). The next run had its first frame 2429 ms after
  the connect, without the keyframe request, and check 4 passed again (probe at `6ba89b2`, 2026-10-29).
