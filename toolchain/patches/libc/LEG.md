# libc leg

- **Upstream:** `libc` 0.2.190 from crates.io.
- **Origin:** bevy-horizon `patches/libc` at `3eac779b2` (MIT OR Apache-2.0), plus punktfunk's
  message calls.
- **Changes:** `src/unix/newlib/mod.rs` and `src/unix/newlib/horizon/mod.rs` only.
  - bevy-horizon: upstream's Horizon types are the 3DS's (32-bit `long`); the leg gives `aarch64`
    Horizon devkitA64 newlib's types, struct layouts and constants, sockets included. Its
    `scripts/check_libc_abi.sh` compares them with the C headers: 0 mismatches at that commit.
  - punktfunk: `msghdr`, `cmsghdr`, `mmsghdr`, the `CMSG_*` functions and `sendmsg`, `recvmsg`,
    `sendmmsg`, `recvmmsg`, from libnx 4.12.0's `sys/socket.h` (FreeBSD's layouts). libnx runs
    the single calls through bsd's batched ones, which need firmware 7.0.0.
- **Applied:** `.cargo/config.toml`, so std's own workspace takes it too (`scripts/cargo-nx.sh`).
- **Re-make:** from the crates.io release Cargo locks, with `diff -r` against 0.2.190 as the recipe.
