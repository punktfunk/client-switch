# quinn-udp leg

- **Upstream:** `quinn-udp` 0.5.16 from crates.io.
- **Origin:** punktfunk.
- **Changes:** `src/lib.rs` builds `fallback.rs` on Horizon instead of `unix.rs`, whose `cmsg`
  path names Linux and BSD socket options libnx does not define. `fallback.rs` (dead upstream on
  every target quinn ships) discards `send_to`'s byte count and, on Horizon, receives up to 32
  datagrams per call through libnx's `recvmmsg` (`BATCH_SIZE` 32). A first refusal other than
  `WouldBlock` drops that socket to one datagram per call with `recv_from` (socket2 has no
  vectored receive on Horizon).
- **Cost:** no ECN, no destination address. The `unix.rs` path comes back behind a cfg once probe
  check 2 shows `sendmsg`/`recvmsg` on the console.
