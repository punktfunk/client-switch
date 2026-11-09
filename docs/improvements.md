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
- **A download page row,** once the repository is public.

## 2. Client only, not done yet

None of these need the monorepo. Each needs a console test or the maintainer's yes first.

### Performance mode: raise the CPU clock

**Why.** At stock clocks the CPU, not the GPU, is the limit. A handheld sweep with sys-clk
(NOTES, 2026-11-02): CPU 1785 MHz took the round trip from 19 ms to 7 ms and end to end from
about 49 ms to 43 ms, and the menu's long frames went away. GPU and memory alone changed little.
The reason is in the next item: the busy core is the system's, not the app's.
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

### Take the picture late in the refresh

Built, with vblank pacing under it, and waiting for its console round: see [Present
path](#present-path-what-changed-and-how-to-test).

**Why.** `Presenter::frame` takes the picture right after `acquire` returns, which the code expects
just after a vblank. A picture decoded moments later misses that take and waits a whole refresh,
even with about 15 ms left before the next vblank. Composing takes about 2 ms, so most of that
wait buys nothing.

**The idea.** While streaming, sleep a few milliseconds after `acquire`, then take the picture. The
same vblank then carries a newer picture, so a picture should reach the panel about that many
milliseconds sooner on average, with no extra dropped pictures. The saving does not depend on how
many images wait in the swapchain. An estimate of 5 to 10 ms, not measured. The idea came from
reading moonlight-vplus's `precise-sync` pacing, which aims each picture at the next vsync using
the display's deadline.

**The test.** A marker file in `/switch/punktfunk/` holding the milliseconds, read at start, with
rounds at none, 4, 8 and 11 ms, handheld, the same game, stock clocks and Smooth 1. `latch` must
become the time `acquire` returned plus a refresh, or the sleep will count as lateness. Then
`slot[10s]`'s late mean shows the gain in the log. It runs about 35 ms today, and a full refresh of
that is the `latch` estimate, not time in the queue.

**Risks.** The console's real deadline is unknown. Past it a picture slips a refresh, a loss the log
may not show, so go up in steps and let the tester's feel count too. That `acquire` returns just
after a vblank is the code's assumption, not a measurement. Filming the screen is ruled out: too
much work for the tester. If the log shows no gain, drop the idea. Needs no monorepo change and no
maintainer's yes for the test.

**Also open.** The stats' display stage and end to end stop when `queue_present` returns, not at
the panel. With three FIFO images, a picture may wait behind one already queued, a hidden refresh
`presenter.rs` does not count. Reasoned from the code, not measured.

### Fewer socket calls per packet: tested, not worth it

**Why it was tried.** At stock clocks the system's core 3 runs at 73 to 90 percent during a stream,
while the app's threads on cores 0 to 2 sit nearly idle (thread probe, 2026-11-03; PR 11, closed).
Every socket call is an IPC served there. Placing or pinning threads cannot help, and the
forwarder option "4 CPU cores" would only add load to core 3 and stays off.

**The test.** Five rounds on 2026-11-04, handheld, CPU 1020 MHz during the stream, Smooth 1, bitrate
Automatic. A build counted every socket call, with two cuts behind marker files: a lighter tokio
wake, and a 1 ms wait before each batch read. Medians over windows at 55 or more pictures a second:

| Round | Calls a second | Datagrams a read | Round trip | End to end |
| --- | --- | --- | --- | --- |
| No cut, first game | 1485 | 12.6 | 10.9 ms | 35.4 ms |
| Lighter wake | 1248 | 12.7 | 11.8 ms | 37.8 ms |
| Read wait | 1404 | 15.6 | 14.7 ms | 34.3 ms |
| Both | 1164 | 15.5 | 14.6 ms | 44.1 ms |
| No cut, second game | 1452 | 13.7 | 13.2 ms | 35.8 ms |

**The result.** The best cut removed 20 percent of the calls, against a target of 50, and the round
trip did not fall. A read costs about 330 microseconds per datagram at every batch size, so the
reader spends about 60 percent of its time in `recvmmsg` whatever the call count. The cost follows
datagrams, about 1870 a second, not calls. That points to a ceiling near 3000 a second, measured as
wall time rather than CPU time. Neither cut shipped. Each setting ran once and no Status Monitor
photo came back, so core 3's load in these rounds is unknown. The code is on the branch
`fewer-socket-calls` (PR 12, closed).

### Pictures in place: the console run

**What.** NVDEC's surfaces go to the presenter as they lie: imported once each through
`VK_EXT_external_memory_host` and detiled (16Bx2 block-linear) by the tiled CSC pass, with no
VIC transfer, no copy and no staging upload (`pf-nx-vk/src/in_place.rs`). Each decoder starts on
the VIC; its first detailed picture is read back through the GPU and compared with the VIC's
copy, and only a match moves the lane in place. A mismatch or a failed import keeps the VIC.

**Test first.** A docked 1080p session: the log's `in place: picture … read back as the VIC
wrote it` line, then `nvdec[10s]` with `route in place`, transfer and copy at 0, and the picture
right. A file `vic-copia` in `/switch/punktfunk/` keeps the VIC route for the A/B.

### NVDEC's top clock in handheld

nvtegra rescales NVDEC every 20 pictures so that one takes about a frame interval
(`av_nvtegra_dfs_update`, floor 180 MHz up to 1080p). Docked, this client holds the top clock;
handheld keeps the scaling, at about 4 ms a 720p picture (NOTES, 2026-10-31). The maintainer
keeps it off by default in handheld (2026-11-05); an opt-in could go with Performance mode.

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

### Socket path: three cuts to test

Each cut removes work from the bsd service on core 3. None has run on the console yet.

- **A wake with no socket call.** While tokio's driver has nothing but its waker registered, as
  during a stream, it sleeps on a condvar. A wake then costs no `send`, `recv` or `poll`, which
  came to about 330, 560 and 320 calls a second of a stream's 1650 (`toolchain/patches/mio`).
  With a socket registered, the loopback datagram still wakes it. **Test:** the count during a
  stream, then the menu, a reconnect and a wake from sleep.
- **Smaller read slots.** Each `recvmmsg` slot offers 2048 bytes rather than the whole 9217-byte
  buffer, and a 28-byte address rather than 128. libnx stages about 66 KB a call rather than
  300 KB. The first datagram that fills a slot is dropped and turns full slots back on for that
  socket. A file `recv-cheio` in `/switch/punktfunk/` keeps full slots, for the A/B. **Test:** a
  read's cost per datagram, against 330 microseconds on 2026-11-04.
- **No spin on a dead socket.** A hard read error waits 10 ms, as core's reader would otherwise
  poll and read again at once. **Test:** a wake from sleep mid-stream.

### Present path: what changed and how to test

None of this has run on the console yet.

- **Pacing on the vblank.** NVK's vi swapchain has three images and its acquire returns as soon
  as the panel frees any, so a loop paced by the acquire keeps two presented frames queued: a
  picture taken after vblank N reaches the panel at N + 2, a refresh no stat counts. That is
  reasoned from Mesa's `wsi_common_switch.c`, not measured. The presenter now takes the display's
  vsync event (`viGetDisplayVsyncEvent`, on a second handle to the default display), clears it
  after each present and acquires only after the next vblank, so at most one frame waits. A
  present that still finds one queued (libnx's `consumer_running_behind`) makes the next frame
  wait two vblanks; if one is still queued after that, the drains stop and the log says so. A
  wait gives up after three refreshes and that frame paces on the acquire; eight in a row pace
  every frame on the acquire. The menu paces the same way. The dev box's offscreen target is
  unchanged.
- **The late take.** While streaming, the presenter sleeps after the vblank until the GPU's
  slowest of its last 32 frames plus 3 ms remain, at most 10 ms into the refresh, then takes
  the picture. `latch` is the vblank after the one waited for, so `slot[10s]` counts the sleep
  as time saved, not as lateness. No sleep until 32 GPU readings exist after a stream starts or
  the swapchain changes.
- **Luma read once at one to one.** A picture the size of the output reads one luma texel a
  pixel in the tiled pass, five fetches instead of eight; chroma keeps its filter. Both paths
  work out the block-linear offset as a row part plus a column part, two of each per 2x2
  footprint. Where both map one to one, the lavapipe test `one_to_one_reads_what_the_filter_reads`
  holds the two paths within 1 of 255; on lavapipe 7 of 96000 bytes differ, from the filter's
  float rounding of the interpolated coordinate.

**The probe line**, every 10 s: `present[10s]: N frames on the vblank, taken late; vblank wait
…; acquire …, N under 1 ms; present …; N found a frame queued, N drained; picture taken … after
the vblank; quick acquires after a stall 0:… 4+:…`.

- `found a frame queued` reads the queue depth directly: nearly every frame if two wait, about
  none if one does.
- `quick acquires after a stall` counts only when paced by the acquire. After a gap of over two
  refreshes the queue is empty, so the acquires that return in under 1 ms before one blocks are
  the free images: two or three if the panel holds one image, one or two if it holds two.
- Paced on the vblank, `acquire` stays under 1 ms; a p50 near a refresh means the queue filled
  anyway. `picture taken` says how far into the refresh the take lands.

**Markers** in `/switch/punktfunk/`: `fila-antiga` paces by the acquire, as v1.0.0 did;
`sem-espera` paces on the vblank without the late sleep. The first `presenter: paced …` line
in the log names the mode, and says so when vi gave no vsync event.

**The test.** Handheld at stock clocks, the same game, Smooth 1: one round each with
`fila-antiga`, with `sem-espera` and with no marker, then the three again docked at 1080p.
Compare `found a frame queued`, the missed vblanks and decode to present in `stats:`,
`slot[10s]`'s late mean, end to end, `compose on the gpu` and the tester's feel. Then a sleep
and a dock change mid-stream with no marker: the picture must come back, with no `paced by the
acquire from here` line.

**Unknown until then:** whether vi hands out the vsync event to a second display handle;
whether docked it follows the TV; whether 3 ms covers the CPU and the present's IPC; and when
the compositor takes its frame for the next vblank. If it takes it soon after the event, a late
present waits a refresh more and the late take loses one. No stat shows that, as decode to
present and end to end stop at the present: only the tester's feel tells `sem-espera` from no
marker there.

## 3. Already done in this client

- **A 20 Mb/s ceiling for Automatic bitrate.** Where only NVDEC decodes, this client sets the
  environment variable `PUNKTFUNK_ABR_MAX_MBPS` that `punktfunk-core::abr` reads
  (`core/model.rs`, `runtime/mod.rs`). On the tester's Wi-Fi each climb to 22 to 25 Mb/s at 720p60
  preceded a delay spike of 60 to 110 ms with no loss, and a pinned 20 Mb/s ran clean (NOTES,
  2026-10-31). Automatic still cuts below the ceiling on congestion.
- **A recvmmsg batch** of up to 32 datagrams per receive call, which moved the console's UDP
  ceiling from about 25 to 33 Mb/s to about 47 to 49 Mb/s.
- **The smoothing slot** skips a standing one-picture backlog.
