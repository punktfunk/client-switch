# mio leg

- **Upstream:** `mio` 1.2.4 from crates.io.
- **Origin:** punktfunk. mio already selects its `poll(2)` backend for Horizon (the 3DS's arm).
- **Changes:**
  - `src/sys/unix/waker/udp.rs` (new): the selector's wake fd is a UDP socket connected to itself
    on loopback; libnx has no `pipe` or `socketpair`. Upstream gives Horizon the single-threaded
    waker, which cannot wake a thread blocked in `poll`, and no `Waker` at all, which tokio needs.
  - `PF_NX_LEAN_WAKER` in the environment makes that waker skip a wake with one on its way and
    ack with one read, for an A/B of the title's socket calls. A wake datagram lost across a
    console sleep would leave it silent until an ack: unproven, so an A/B round does not sleep.
  - `Waker` and `Selector::wake` compiled for Horizon (`lib.rs`, `poll.rs`,
    `sys/unix/selector/poll.rs`); `sys/unix/mod.rs` picks `waker/udp.rs` on `aarch64` Horizon.
  - `PollFlagInt` is `c_short` on `aarch64` Horizon, as devkitA64's `pollfd` (the libc leg).
  - `sin_len` in `sys/unix/net.rs`'s `sockaddr_in`, which devkitA64's has.
- **Unproven:** the UDP waker on the console; probe check 3 runs tokio on it.
