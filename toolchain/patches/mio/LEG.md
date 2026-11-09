# mio leg

- **Upstream:** `mio` 1.2.4 from crates.io.
- **Origin:** punktfunk. mio already selects its `poll(2)` backend for Horizon (the 3DS's arm).
- **Changes:**
  - `src/sys/unix/waker/udp.rs` (new): the selector's wake fd is a UDP socket connected to itself
    on loopback; libnx has no `pipe` or `socketpair`. Upstream gives Horizon the single-threaded
    waker, which cannot wake a thread blocked in `poll`, and no `Waker` at all, which tokio needs.
  - `sys/unix/selector/poll.rs`: while only the waker is registered, `select` sleeps on a
    condvar, and a wake is a flag and a signal: no socket call. Only a `select` inside `poll` gets
    the datagram, and a wake already pending sends none. A registration returns `select` empty.
    Tests: `RUSTFLAGS='--cfg mio_unsupported_force_poll_poll' cargo test --offline --lib
    --features os-poll,net` in a copy of this directory without its `Cargo.lock`.
  - `Waker` and `Selector::wake` compiled for Horizon (`lib.rs`, `poll.rs`,
    `sys/unix/selector/poll.rs`); `sys/unix/mod.rs` picks `waker/udp.rs` on `aarch64` Horizon.
  - `PollFlagInt` is `c_short` on `aarch64` Horizon, as devkitA64's `pollfd` (the libc leg).
  - `sin_len` in `sys/unix/net.rs`'s `sockaddr_in`, which devkitA64's has.
- **Unproven:** the UDP waker and the condvar sleep on the console; probe check 3 runs tokio on it.
