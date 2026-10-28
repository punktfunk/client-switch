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
| `pf-nx-host` | the seams on a dev box (Vulkan on Linux, CPU raster on macOS) | none |
| `pf-nx-probe` | WP0's numbered checks, its own NRO | none |

The app never names a libnx call or a Vulkan handle; the binary hands it a `Platform`.

## Invariants

- The title never returns from `main` and never calls `exit`: `Lifecycle::exit` loads `"exit"`.
- Every thread is a std thread; libnx's `threadCreate` is wrapped by the target (`toolchain/`).
- Threads are never named from inside themselves.
- A console claim in `docs/NOTES.md` cites a bundle id. No workaround without a probe line.
- Writing rules are the monorepo's `docs/writing.md`: commits, comments, the two error registers.
