# quinn-udp leg

- **Upstream:** `quinn-udp` 0.5.16 from crates.io.
- **Origin:** punktfunk.
- **Changes:** `src/lib.rs` builds `fallback.rs` on Horizon instead of `unix.rs`, whose `cmsg`
  path names Linux and BSD socket options libnx does not define. `fallback.rs` (dead upstream on
  every target quinn ships) discards `send_to`'s byte count and, on Horizon, receives up to 32
  datagrams per call through libnx's `recvmmsg` (`BATCH_SIZE` 32). A first refusal other than
  `WouldBlock` drops that socket to one datagram per call with `recv_from` (socket2 has no
  vectored receive on Horizon). Each slot offers 2048 bytes and a 28-byte address, as libnx
  stages every slot whole per call. The first datagram that fills a slot comes back empty and
  turns full-size slots on for that socket; `sdmc:/switch/punktfunk/recv-cheio` keeps them on.
  A hard error waits 10 ms before it returns, so a dead socket's reader does not spin.
- **Cost:** no ECN, no destination address. The `unix.rs` path comes back behind a cfg once probe
  check 2 shows `sendmsg`/`recvmsg` on the console.
