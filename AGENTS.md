# AGENTS.md

Nintendo Switch homebrew client for [punktfunk](https://git.unom.io/unom/punktfunk): a Rust title
on Vulkan (Mesa NVK, linked statically), the shared console UI and NVDEC through libavcodec's
`nvtegra` hwaccel. Plan of record: `docs/plan.md`. Console facts and what is proven: `docs/NOTES.md`.

## Layout (dependencies point down)

| Crate | What | `unsafe` |
| --- | --- | --- |
| `pf-nx-app` | session, console hosting, services, run loop, **seams** (`platform/`) + fakes | forbidden |
| `pf-nx-vk` | device, presenter, UI layer, PyroWave lane (Linux + console only) | with `// SAFETY:` |
| `pf-nx-sys` | hand-written libnx / libavcodec / NVK FFI, layout tests, newlib shims | with `// SAFETY:` |
| `pf-nx-platform` | the console's seam implementations (pure maps build anywhere) | via sys/vk |
| `pf-nx-host` | the seams on a dev box (Vulkan on Linux, CPU raster on macOS) | one loader call |
| `pf-nx-probe` | WP0's numbered checks, its own NRO on `pf-nx-sys` (`av` behind `nvdec`, NVK behind `vulkan`) | with `// SAFETY:` in `vk.rs` |

The app never names a libnx call or a Vulkan handle; the binary hands it a `Platform`.

## Invariants

- The title never returns from `main`: `Lifecycle::exit` is the one way out, and it runs after
  every worker thread has joined (a thread still running holds a locked stack the loader faults on).
- Every thread is a std thread; libnx's `threadCreate` is wrapped by the target (`toolchain/`).
- Face buttons go by position on the wire: Nintendo A (east) is `BTN_B`. A confirms, B backs.
- libnx has no suspend hook: sleep is `FocusState::Background`, wake is `Resume`.
- Threads are never named from inside themselves.
- A console claim in `docs/NOTES.md` cites a bundle id. No workaround without a probe line.
- Writing rules are the monorepo's `docs/writing.md`: commits, comments, the two error registers.
