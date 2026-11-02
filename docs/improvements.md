# Improvements

Changes the console's runs point to, for later. Each needs the maintainer's yes before it lands:
the first changes what a player sees by default, the others live in punktfunk's shared crates.

## The smoothness buffer's Automatic on the Switch

The shared setting resolves Automatic to two frames on every client
(`pf-client-core::trust::PresentPriority::resolve`). This client counts core's adaptive cushion
as the first frame and adds a stream period for each further one (`session/timeline.rs`). Until
2f4002a it added every frame on top of the cushion, so one frame held up to about two
refreshes. On the console, one frame presented 59–60 pictures a second at 15–23 ms decode to
present; two frames presented the same at 31–40 ms (NOTES, 2026-10-31). Both runs predate
2f4002a. The proposal is that Automatic picks one frame on the Switch, through a per-platform
default in the shared crate or an override in this client.

Open: one tester's Wi-Fi so far. A busier network may want the second frame back.

## A ceiling for the automatic bitrate

Automatic bitrate climbs to the link's wall and asks it again every re-probe interval. On the
tester's Wi-Fi each climb to 22–25 Mb/s at 720p60 preceded a one-way delay spike of 60–110 ms
with no loss, a queue upstream of the console. Pinning the bitrate at 20 Mb/s (16–17 Mb/s
delivered) removed the spikes for a whole session, but a pinned rate does not come down when the
link gets worse (NOTES, 2026-10-31).

The middle ground is automatic under a ceiling the player or the platform sets, so it still cuts
on congestion but never probes past what a handheld's Wi-Fi carries. The controller is core's
(`punktfunk-core::abr`), so this is a change upstream, not here.

Open: what the ceiling should be docked at 1080p, which needs more than 720p.

## The Nintendo controller types in the shared lists

On the Switch a player holding Joy-Cons can ask the host for a Pro Controller only through the
dial: the Switch's own step (`GamepadType::next_on_dial`) goes Automatic, Joy-Con pair, Switch
Pro, then the shared order. Everything else about the two kinds lives in the shared crates and
leaves them out:

- Settings' Controller type offers Automatic, Xbox 360, Xbox One, DualSense, DualShock 4, Steam
  Deck and Steam Controller 2 (`pf-console-ui`, `screens/settings/rows.rs`, `PAD_TYPES`), so the
  console cannot save either Nintendo kind.
- The dial names a kind through `pf-client-core::overlay_actions::pad_type_label`, which has no
  name for either, so the slot reads `joyconpair` and `switchpro`.
- The dial's shared order (`overlay_actions::PAD_TYPE_CYCLE`) has neither, which is why the
  Switch steps its own.

The proposal is a monorepo PR that adds "Joy-Con pair" and "Switch Pro" to the labels, to the
Settings list on the platforms that present them, and to the shared order there, after which
the Switch's own step goes. The Windows host built a Joy-Con pair in every console session
(NOTES, 2026-10-31); a Switch Pro there is untested.

Open: whether every host can build a Switch Pro, or the lists should offer it per host.

## A Switch platform in the shared shell

The shell has no Switch, so this client fronts it as `Platform::Ps5` (`runtime/menu.rs`), the
nearest: a console title that quits, with no window. That borrows what is the PS5's alone. The
tester found the first on 2026-11-02:

- **Stream controls.** About > Stream controls draws a DualSense card that lists "Touchpad +
  Cross" and "L1 + R1 + Options + Touchpad". On the Switch the dial is Minus + A
  (`core/dial.rs`), and holding L + R + Plus + Minus for a second ends the stream
  (`runtime/pads.rs`, `CHORD_HOLD`). The page tells a player to press buttons that do nothing.
- **Confirm side.** The shell confirms on south. Here A, on east, confirms, so `legend_pref`
  (`runtime/menu.rs`) swaps the Nintendo legend for the letters one. Without that, the hints
  read "B Select" over the button that selects.

The contract this client builds against, in the monorepo:

1. `pf-console-ui::platform::Platform::Switch`, in `Platform::ALL`, with `can_quit` true.
2. `pf-client-core::shortcuts::Client::Switch`, mapped from it in `screens/controls.rs`. Its pad
   group is titled "Joy-Con" and holds:
   - `item("Minus + A", "Open the quick actions dial")`;
   - `item("L + R + Plus + Minus", "Hold to disconnect")`.

   The group's icon and badges come from the first pad's pref, as now. The pad art for
   `joyconpair` and `switchpro` already exists (`pad_art.rs`).
3. On `Platform::Switch`, the Nintendo glyph style confirms on east and backs on south. Every
   hint that says Select or Back follows that.
4. The rest of the PS5's branches, decided for the Switch:

   | Where | PS5 now | Switch |
   | --- | --- | --- |
   | `codecs` | `CODECS_PS5`: no AV1, PyroWave behind `pyrowave_ok` | the same. NVDEC has no AV1, and PyroWave runs on NVK's Vulkan 1.3, untested on the console (`pyrowave: false` until then) |
   | `pad_types` | drops Xbox 360, Steam Deck, Steam Controller 2 | the same, plus the Nintendo kinds from the section above |
   | `own_stats_corner` | top left | top left |
   | `has_window` | false | false |
   | `bitrate_ceiling_kbps` | the custom maximum | the custom maximum |
   | `ring_editor` platform | Desktop | Desktop |
   | `GlyphStyle::keys` | Remote | Remote |
   | help-text branches (mouse mode, stats verbosity, gamepad UI) | the no-chord wording | the same |

   `players.rs` opens a pad's menu (Test controller, Rumble) on Android and Apple only
   (`can_test`, `can_rumble`). On every other platform, OK on a Controllers card does nothing, so
   the Switch's Joy-Con card is display-only. The Switch can do both: its pads rumble in every
   stream. The contract lets `Platform::Switch` offer the two rows. This client then answers
   `ConsoleCmd::PadTest` and the `rumble` `PadAction`, which `ui/service.rs` logs and drops
   today.

   The handheld screen is a touchscreen, and this client sends its touches to the host as a
   direct pointer. That is a later row, not part of this contract.

On this side, once the client builds on a release that has it:

- `runtime/menu.rs` sets `platform: Platform::Switch`;
- `legend_pref` goes, and the pads report their own pref;
- both `TODO(core)` lines go.

Until then, the guide names the two chords, because the Stream controls page does not.

Open: whether the shell should take the dial and escape chords from the client, not from a
table, so a client that rebinds them cannot drift from the page.
