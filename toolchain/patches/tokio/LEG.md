# tokio leg

- **Upstream:** `tokio` 1.54.1 from crates.io.
- **Origin:** punktfunk.
- **Changes:** `src/net/unix/ucred.rs` gives Horizon the no-process `get_peer_cred` arm, as the
  Vita has; upstream names no arm for Horizon and fails to compile with `net`. The client never
  opens a Unix socket.
