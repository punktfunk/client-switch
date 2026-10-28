# Console notes

Facts about the console and what is proven, each citing a bundle id. Nothing yet.

## Toolchain

What builds, and how; nothing here has run on a console.

- `scripts/docker.sh` runs a command in the image on a copy of the tree in the `pf-nx-src`
  volume (no bind mount); `target/` and cargo's registry live in volumes, `dist/` comes back out.
- `scripts/cargo-nx.sh check|build -p pf-nx-probe` cross-builds; `scripts/package-nro.sh
  pf-nx-probe` writes `dist/pf-nx-probe.nro`. Checks 1–2 build with `--no-default-features`.
- Check 3 (`quic`) builds only with punktfunk-core skipping `if-addrs` on Horizon, which the pinned
  rev does not (plan §7, PR 2): the Cargo.toml target block and the three call sites
  (`discovery.rs`, `wol.rs`, `transport/ifinfo.rs`) take the wasm arm on `target_os = "horizon"`.
- aws-lc builds with its CC builder: no BIO sockets, CPU features from `-march`, getentropy for
  entropy (`scripts/cargo-nx.sh`). rustls's provider is aws-lc-rs; ring is not linked.
- quinn-udp runs its fallback (no ECN, one datagram per call) until the console shows `sendmsg`.
- tokio's I/O driver wakes through a loopback UDP socket (the mio leg): unproven on the console.
- devkitPro's `switch-ffmpeg` in the image carries the `nvtegra` hwaccel (`libavutil/
  hwcontext_nvtegra.h`; the symbols are in `libavcodec.a`), so plan D6 needs no FFmpeg fork.
  The portlibs also hold `libopus.a`, `libEGL.a` and `libdrm_nouveau.a`.
- `pf-nx-sys` cross-clippies clean for the console without features; with `av` the C shim
  compiles against the image's libavcodec headers.
- The whole title type-checks and links for the console: `scripts/cargo-nx.sh build -p pf-nx`
  then `scripts/package-nro.sh pf-nx punktfunk` writes `dist/punktfunk.nro` (78 MB: the image's
  full libavcodec with its dav1d and swresample closure, Skia, NVK, line tables). Until plan §7's
  monorepo PRs land, the build needs the patched shared crates at `/opt/pf-nx/mono`
  (`scripts/cargo-nx.sh` adds the patch table to the image copy's config).
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
- Check 4 (`vulkan`) has not run anywhere: Ryujinx does not run NVK builds (plan D15).
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

The monorepo change check 3 needs, as a diff against the pinned rev: `docs/monorepo-horizon.diff`
(plan §7, PR 2). It goes away when that PR lands and the pin moves.
