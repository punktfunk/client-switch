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
