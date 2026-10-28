# getrandom 0.4 leg

- **Upstream:** `getrandom` 0.4.3 from crates.io.
- **Origin:** bevy-horizon `patches/getrandom` at `3eac779b2` (MIT OR Apache-2.0).
- **Changes:** a `horizon` + `aarch64` arm in `src/backends.rs` and `src/backends/horizon.rs`,
  which fills from newlib's `arc4random_buf`; the probe's `_getentropy_r` backs that with libnx's
  `randomGet`. Upstream fails to compile for the Switch.
- **Used by:** `rand` 0.10 (quinn-proto).
