# getrandom 0.2 leg

- **Upstream:** `getrandom` 0.2.17 from crates.io.
- **Origin:** bevy-horizon `patches/getrandom-0.2` at `3eac779b2` (MIT OR Apache-2.0).
- **Changes:** `src/lib.rs` and `src/util_libc.rs` take newlib's `getrandom()` and `__errno()` on
  any Horizon target; upstream does only on the 3DS. The probe's `getrandom` backs it with
  libnx's `randomGet`.
- **Used by:** `rand_core` 0.6 (spake2's curve25519). Patched under the key `getrandom02`.
