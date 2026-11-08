# pyrowave-sys leg

- **Upstream:** `crates/codec/pyrowave-sys` of the monorepo at `PUNKTFUNK_REV`
  (`b58dc3cfe152c48c584c8e2f77dc05d08f6aeb12`), version 0.46.0. The vendored PyroWave, Granite,
  volk and Vulkan-Headers pins are in `vendor/pyrowave/PUNKTFUNK-VENDOR.txt`.
- **Origin:** punktfunk. The monorepo builds this crate for Linux, Windows, 64-bit Android and the
  PS5 (`8880bcd5e`); on Horizon its build script writes empty bindings, and `pf-client-video`'s
  `video_pyrowave`, which already builds for Horizon, finds no C API.
- **Changes:**
  - `build.rs` builds on Horizon, with `-I horizon -include horizon/pf_nx_time.h
    -ftls-model=local-exec` on the C and C++ flags. The C++ runtime comes from the title's link
    (`pf-nx-sys`), as on the PS5. The TLS model is devkitPro's own (`Switch.cmake`), whose flags
    cmake-rs replaces; without it Granite's first `thread_local` store faulted when a PyroWave
    stream opened (Atmosphère report `01794144931`, 2026-11-08).
  - `horizon/dlfcn.h` (new): every lookup fails. Only volk includes it, and Granite takes the
    title's `vkGetInstanceProcAddr` (`pyrowave_create_device`), so nothing is ever loaded.
  - `horizon/pf_nx_time.h` (new): `clock_nanosleep`, which newlib lacks, as `nanosleep` on the
    time left. Granite's `Timer::sleep_until` is its one caller.
  - `build.rs` renames volk's global `vk*` function-pointer variables to `pf_volk_*` in every
    archive of the crate (`objcopy --redefine-syms`). NVK, linked statically, defines the same
    names as functions, and the title's `vkGetInstanceProcAddr` bound to volk's variable: the
    first launch aborted jumping into it (Atmosphère report `01794143733`, 2026-11-08).
  - `vendor/pyrowave/Granite/third_party/renderdoc/renderdoc_app.h` names `__SWITCH__` beside
    `__APPLE__` for its calling convention; it stopped the build with "Unknown platform".
  - `Cargo.toml` spells out the workspace fields it inherited and keeps only the `sie` cfg lint.
- **Unproven:** the decoder on NVK on the console. The leg goes once the monorepo builds this
  crate for Horizon.
