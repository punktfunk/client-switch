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
  `__normal_iterator`. Off an arm64 box that needs Debian's arm64 cross libstdc++ in the image
  (CI's x86_64 runner had no `cassert`). `toolchain/skia/layout-check.py` holds G++'s sizes and alignments to
  bindings.rs on every build: none differ (raster 352 of 383 types, vulkan 390 of 426; the rest
  are private or anonymous).
- On a box that is not Linux the dev box runs in `docker/host.Dockerfile` (`task docker:host-test`;
  `PF_NX_IMAGE=$HOST_IMAGE scripts/docker.sh …` for anything else). `pf_av.c` builds there against
  the system's libavcodec and decodes in software, so `pf-nx-host` streams H.264/HEVC through the
  console's video lane into the presenter on lavapipe. docker.sh drops ignored files from the
  source volume each run: the dev box's data dir lives under `target/` (`--data-dir /w/target/devbox`).
  mDNS does not reach the container, so a test run names the host and its management port
  (`mgmt_port`), which a host can move off 47990.
- `task cross-check` type-checks the title and the probe; the title takes the Vulkan Skia
  archive (`task skia -- vulkan`). CI keeps that archive in its cache, as it does NVK's.
- Either archive leaves one symbol to the title, `sysconf` (pf-nx-sys has it), and needs
  `-lstdc++`, which pf-nx-sys links only with `nvk`. `kN32` is RGBA: Linux's BGRA comes from
  `is_linux` alone. `FontMgr::new()` is the empty manager; the title's fonts load from data.

## Proven on the console

Runs on the tester's Switch 1 (Atmosphère, from hbmenu). No bundles yet: each item cites the
probe build's commit and the run's date.

The tester's console runs an overclock profile: the CPU at 1785 MHz is above stock, the GPU at
307 MHz and EMC at 1600 MHz are what the profile left. The runs before 2026-10-29 12:30 do not
record their clocks; the numbers below are under that profile unless an item says otherwise.

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
- Check 6 (`nvdec`) passes: 1080p60 H.264 and HEVC from `sdmc:` (no B-frames, one IDR, ten
  seconds each, `scripts/nvdec-streams.sh`) through `nvtegra`, one AU at a time. Every AU came
  out as a picture with one in flight; the decoder opened in 1 ms and the first picture took
  8 ms. Send to picture out averaged 7.7 ms for H.264 (8.7 ms worst) and 7.2 ms for HEVC
  (8.3 ms), the VIC transfer to NV12 2.7 ms (3.2 ms), and the whole loop with the copy into the
  caller's planes ran 87.9 and 92.0 fps. The first pictures' mean luma is 126, so they are not
  blank. 10-bit HEVC opens, but receiving its first picture fails with `EINVAL` (-22); the shim
  asks the transfer for NV12 (probe at `d69e5a4`, 2026-10-29).
- The probe stays on its last screen until it is closed from HOME: a run that ends looks the
  same as one that hangs. Only the `probe end` line tells them apart.
- Check 4's swapchain half passes: `VK_NN_vi_surface` on libnx's default window (1280x720 in
  handheld, the current extent, 3 images exactly), a FIFO `B8G8R8A8_UNORM` swapchain, a 256x256
  texture uploaded through a staging buffer in 4.8 ms, and the presenter's committed SPIR-V
  (`fullscreen.vert`, `overlay.frag`) building on NVK. 600 frames of a clear plus a textured quad
  presented at 60.30 fps: present-to-present median 16.67 ms, 99th percentile 16.84 ms, worst
  18.56 ms, 596 of 599 intervals within 14–19 ms and none over 19 ms; 55 ms from surface to the
  first frame. The tester saw the animation smooth. Clocks before and after: GPU 307 MHz, CPU
  1785 MHz, EMC 1600 MHz (probe at `73f3ca8`, 2026-10-29).
- In that run the text console, opened again after the swapchain was destroyed, printed check 6's
  lines, then libnx aborted the title at the next line: `framebufferBegin` →
  `LibnxError_BadGfxDequeueBuffer` (2345-0021) from `nwindowDequeueBuffer` (Atmosphère report
  `01793287821_05446530aca7e000`). The probe now runs check 4 last and leaves the console closed.
- At stock clocks (GPU 307 MHz, CPU 1020 MHz, EMC 1331 MHz before and after check 4) the probe
  runs to `probe end` with check 2 the only failure, and no crash on HOME. Check 3's first frame
  took 2457 ms. Check 6 ran 78.8 fps for H.264 (decode 8.0 ms, 10.5 ms worst; transfer 3.4 ms,
  4.2 ms) and 82.7 fps for HEVC (7.4 ms, 9.1 ms; 3.4 ms, 3.8 ms), every AU a picture. Check 4
  presented 600 frames at 60.23 fps: median 16.67 ms, 99th percentile 16.85 ms, worst 17.04 ms,
  596 of 599 intervals within 14–19 ms; 41 ms from surface to the first frame. The tester saw
  the animation smooth (probe at `70a80e5`, 2026-10-29).
- The title's first launch logs its start line (Erista, firmware 22.5.0, NVK on the GM20B, Skia on
  Vulkan at 1280x720) and stamps `settings.json`, then dies building the console UI: a data abort
  reading address 0 in `SkSL::Pool::AllocMemory`, under the backdrop's `SkRuntimeEffect::
  MakeForShader`. Skia's `-fPIC` makes its thread-locals global-dynamic, and devkitA64's GCC 16
  under `-mtp=soft` compiles that to a load from tp + tp with no TLS relocation; the SkSL pool,
  the strike cache, `AtlasTextOp` and libjpeg-turbo's SIMD flags all had it. The Skia wrappers now
  pass `-ftls-model=local-exec`, and `scripts/build-skia.sh` refuses an archive that still has the
  pattern (title at `d484d57`, 2026-10-30; Atmosphère report `01793372003_05446530aca7e000`).
- With that archive the title boots on NVK to the console UI: the tabs warm in 647 ms, mDNS
  resolves the PC's host on the LAN, and the menu drew 586 frames in 10 s at a CPU p50 of 2.8 ms
  (p99 5.4 ms). Quitting from the menu then faulted hbl, not the title: a user break while it
  loaded the next NRO (2001-0106), the title's threads all gone. Closing from HOME did not
  (title at `5b7dfa2`, 2026-10-30; Atmosphère report `01793385393_05446530aca7e000`).
- The title paired with the host by PIN (its identity is its own, not the probe's), streamed
  720p60 HEVC through NVDEC, came back to the menu, and quit: hbl faulted the same way, so
  dropping the display, the decoders and the pads before `Lifecycle::exit` was not enough. The
  settings writer failed every save after the first: `rename` onto an existing file is EEXIST
  on Horizon (title at `a29f5d6`, 2026-10-30; Atmosphère report `01793390067_05446530aca7e000`).
- libnx 4.12 leaves `pthread_detach`'s back end undefined, so a detached std thread that ended
  kept its stack mapped and its heap pages locked; bevy-horizon names that as one cause of the
  same hbl fault. `pf-nx-sys::rt` now closes them, `rename` replaces, and the exit writes
  `exit.log` with what still holds the heap and leaves through AM to HOME if anything does.
- With that, quitting from the menu returns to hbmenu, three times in a row, the test run's
  included: `exit.log` reads one detached thread closed and the heap clean at once. The test
  run (`menu`) drew the Hosts screen, moved right and back, and wrote its verdict with three
  frames. The host started unpaired and paired again by PIN; then a 720p60 HEVC stream through
  NVDEC, back to the menu, a renamed host, the client's name and a setting changed, and all
  three still there on the next launch, with no failed save. Of 23 ten-second windows in the
  menu, 14 drew 595–601 frames; the other 9 drew 567–593, mostly with a frame of 65–213 ms
  (title at `927533b`, 2026-10-30; bundle `verdict/1793400050`).
- Docking mid-stream re-sizes without a redial, both ways: message 30 arrives, the window goes
  to 1920x1080 (`nwindowSetDimensions` takes it once the old swapchain is gone), a 1920x1080
  swapchain follows, the stream asks for 1920x1080@60, and the host delivers it in 1.2 s with
  NVDEC reopened; undocking does the same at 1280x720. One loss froze 159 ms at the dock. Docked
  at 1080p NVDEC decoded in 7.6 ms mean (10.7 ms worst) and the copy out took 3.7 ms, against
  4.0 ms and 1.7 ms at 720p; 30 s docked so far, not the ten minutes WP3 asks
  (title at `b8e2986`, 2026-10-31).
- Under the smoothness intent with one frame and the bitrate pinned at 20 Mb/s (16–17 Mb/s
  delivered), five minutes handheld presented 59–60 pictures a second, with 0–3 a window dropped
  unseen and none trimmed outside the two mode changes; decode to present ran 15–23 ms p50 and
  end to end 32 ms p50, 49 ms in the windows a refresh later. No one-way-delay cut fired. The
  latency intent on the same Wi-Fi presented 47–58 a second at 5–6 ms decode to present, and in
  the earlier runs the automatic bitrate's climbs to 22–25 Mb/s preceded every 60–110 ms delay
  spike (title at `b8e2986` and `517083f`, 2026-10-31).
- Sleep mid-stream crashes the title on wake. Nothing reached the log between the last stats
  line and the wake 40 s later, so the title went to sleep without a word from the applet. On
  wake the stream ended on a present error, and the teardown's first wake of tokio's I/O
  driver failed: the loopback waker's send errs after sleep, tokio's `expect` panicked, the
  panic hook's backtrace aborted (the title builds without unwind tables, so it never logged
  the panic), and the logger thread wrote to `sdmc:` after `__appExit` had unmounted it: a data
  abort at 0x20, `_write_r` through a null devoptab. The host saw the session drop and gave the
  PC its display back (title at `b8e2986`, 2026-10-31; Atmosphère report
  `01793482713_05446530aca7e000`). The hook now flushes the message before the walk, and the
  waker leg treats a failed send as a wake that reached no one.
- Launched from the album (hbloader in applet mode) the title died the same way, a data abort
  at 0x20 in `_write_r` from the logger thread after `__appExit`, so something panicked first;
  the title log was lost to the next launch, which truncates it. The console needed a reboot
  into the CFW afterwards. A second sleep mid-stream on the same build ended as the first
  (title at `b8e2986`, 2026-10-31; Atmosphère reports `01793488062_010000000000100d` and
  `01793488512_05446530aca7e000`). Applet mode is left untested until a build that logs its
  panics runs there.
- On the WP4 build two sleeps mid-stream (37 s and 28 s) no longer crash the title. Again no
  applet message or hook arrived before sleep; on wake the session's socket read failed with
  "Network interface is not configured" (os error 115), the session ended as lost, and the
  stream redialled the host and was back in 0.3 s, so the player lands in the game, not on
  Hosts (plan D12 expected Hosts). The touch screen drives the host's desktop as touch, the
  host's rumble reaches the Joy-Cons, and the gyro turns a game's camera the right way on the
  pitch and yaw axes. hid's accelerometer reads gravity's pull, [0, 0, -1] g lying still,
  which the first build sent upside down as [0, -1, 0]. audout reports volume 1.0; the host's
  audio peaked at -4 to -12 dBFS in play, so the level arrives near full scale. Audio
  underruns reached 47 in the first session's three minutes and 38 in the last one's twenty
  (title at `be98e16`, 2026-10-31).
- With the accelerometer's sign flipped, a pad lying still reads [0, 1, 0] g on the wire, as
  SDL's frame has it (title at `8a18316`, 2026-10-31).
