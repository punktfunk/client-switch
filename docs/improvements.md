# Improvements

What is left after v1.0.0, sorted by who can do it:

1. [**Needs a change in the punktfunk monorepo**](#1-needs-a-change-in-the-punktfunk-monorepo):
   a maintainer of the main repository has to write it. This client cannot do it alone.
2. [**Client only, not done yet**](#2-client-only-not-done-yet): this repository can do it, but
   each item needs a console test first, or the maintainer's yes because it changes what a
   player sees.
3. [**Already done in this client**](#3-already-done-in-this-client): kept here so the monorepo
   side knows what exists.

Facts about the console cite `docs/NOTES.md` by date.

## How this client uses the monorepo

The monorepo is `unom/punktfunk` (<https://git.unom.io/unom/punktfunk>). This client takes four
of its crates, `punktfunk-core`, `pf-client-core`, `pf-console-ui` and `pf-client-video`, at **one
git rev**, set in `Cargo.toml` and `scripts/pins.env` (`PUNKTFUNK_REV`).

The usual round trip for section 1:

1. A monorepo PR lands the change. The monorepo's `docs/writing.md` has the rules for commits,
   comments and error text; read the monorepo from `origin/main`, never from a local checkout.
2. This client bumps the rev (`docs/pins.md`, row "punktfunk": one bump moves all four crates)
   and deletes the workaround named in each item below.
3. A console round checks it, and the release notes say so.

## 1. Needs a change in the punktfunk monorepo

- **A Switch platform in the shared shell.** The shell has no Switch, so this client presents
  itself as the PS5. The Stream controls page then tells players to press DualSense buttons, and
  this client patches A and B (confirm and back) on its own. The shell should know the Switch,
  its two chords (Minus + A, and L + R + Plus + Minus) and A as confirm.
- **The Nintendo controller types in the shared lists.** Settings cannot save "Joy-Con pair" or
  "Switch Pro", and the dial shows their internal ids. The shared lists and labels should include
  them. Open: can every host build a Switch Pro?
- **A dead connection leaves a thread spinning.** Unplugging the dock mid-stream leaves the old
  receive thread running until the app exits, wasting CPU (`punktfunk-core` transport). A fatal
  socket error should end it.
- **Fewer socket calls per packet.** The system's core 3 saturates at stock clocks serving socket
  calls (section 2, Performance mode). This client already reads 32 datagrams a call; the rest is
  in `punktfunk-core`'s receive loop.
- **A download page row,** once the repository is public.

## 2. Client only, not done yet

None of these need the monorepo. Each needs a console test or the maintainer's yes first.

### Performance mode: raise the CPU clock

**Why.** At stock clocks the CPU, not the GPU, is the limit. A handheld sweep with sys-clk
(NOTES, 2026-11-02): CPU 1785 MHz took the round trip from 19 ms to 7 ms and end to end from
about 49 ms to 43 ms, and the menu's long frames went away. GPU and memory alone changed little.
A thread probe (2026-11-03, handheld, stock clocks; PR 11, closed) shows why: the app's threads
already sit on cores 0 to 2, nearly idle (13, 9 and 6 percent at CPU 1785 MHz), while core 3, the
system's, runs at 73 to 90 percent serving every socket call. So placing or pinning threads
cannot help, and the forwarder option "4 CPU cores" would only add load to core 3 and stays off.
A 27-minute session at CPU 1785 MHz reached 30 ms end to end.

**The idea.** An opt-in setting, off by default: a small CPU raise in the menu, and CPU 1785 MHz
(maybe memory 1600) while streaming. Only after it can Automatic's 20 Mb/s ceiling go up to about
25 Mb/s.

**Test first** (AGENTS.md: no workaround without a probe line):

- whether `clkrstSetClockRate` works from an NRO under hbloader;
- whether sys-clk overrides it;
- whether the clocks come back on exit, on sleep (`FocusState::Background`) and on a dock change.

**Constraints.** Do not use `appletSetCpuBoostMode`: it drops the GPU to 76.8 MHz, which would
hurt composition and the UI. Stay within the official top clocks (CPU 1785, memory 1600). Restore
before `Lifecycle::exit`. The cost is battery and heat. **Needs the maintainer's yes**, because it
adds a player-facing setting (`docs/settings.md`).

### The smoothness buffer's Automatic on the Switch

The shared setting resolves Automatic to two frames on every client
(`pf-client-core::trust::PresentPriority::resolve`). This client counts core's adaptive cushion as
the first frame and adds a stream period for each further one (`session/timeline.rs`). On the
console, one frame presented 59 to 60 pictures a second at 15 to 23 ms decode to present; two
frames presented the same at 31 to 40 ms (NOTES, 2026-10-31).

**The proposal.** Automatic picks one frame on the Switch, either through a per-platform default
in the shared crate (section 1 would carry it) or an override here. **Needs the maintainer's
yes**: it changes what a player sees by default. **Open:** one tester's Wi-Fi so far; a busier
network may want the second frame back.

### Other later work

- **PyroWave on NVK.** The lane is lifted but off. A 256-core Maxwell GPU at 307 to 460 MHz has
  to be measured against the wavelet decode before it is advertised (plan D14).
- **USB keyboard and mouse on the dock** (`hidKeyboard*` and `hidMouse*` into the keymap
  contract).
- **The console's `hwopus` service** as an audio lane.
- **Longer tests:** a second pad, ten minutes docked at 1080p, an hour of play.
- **Applet mode.** A launch from the Album crashes before the first menu. Not investigated; the
  manual says to hold R over a game.
- **Audio** plays 20 to 55 ms behind the picture, steady.
- **The Homebrew App Store listing** and the **Switch 2** (plan §9).

## 3. Already done in this client

- **A 20 Mb/s ceiling for Automatic bitrate.** Where only NVDEC decodes, this client sets the
  environment variable `PUNKTFUNK_ABR_MAX_MBPS` that `punktfunk-core::abr` reads
  (`core/model.rs`, `runtime/mod.rs`). On the tester's Wi-Fi each climb to 22 to 25 Mb/s at 720p60
  preceded a delay spike of 60 to 110 ms with no loss, and a pinned 20 Mb/s ran clean (NOTES,
  2026-10-31). Automatic still cuts below the ceiling on congestion.
- **A recvmmsg batch** of up to 32 datagrams per receive call, which moved the console's UDP
  ceiling from about 25 to 33 Mb/s to about 47 to 49 Mb/s.
- **The smoothing slot** skips a standing one-picture backlog.
