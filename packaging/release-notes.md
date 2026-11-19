punktfunk for the Nintendo Switch: stream games from your PC to a Switch 1 running Atmosphère.

## What's new in 1.2.0

- **PyroWave, a new low-latency codec (experimental).** Pick it as the codec in the settings. Around
  30–35 Mb/s its delay matches HEVC's or comes in a few milliseconds under; the picture is
  softer. Higher bitrates add delay, as the Switch's Wi-Fi can't take the packets in fast
  enough, and from about 80 Mb/s the stream drops frames. Tested with the CPU clock raised.
- **Less waiting before the screen.** In a stream, each frame goes out on the display's refresh
  with at most one other waiting, where two could wait before. The app also makes fewer network
  calls, leaving the console more room while streaming.
- **A smooth menu again.** Since the new stream pacing the menu could hitch when changing tabs;
  it now keeps the same smoothness as 1.0.0.
- **Your own shortcuts.** The Shortcuts page now shows Switch buttons and can record a chord
  you press.
- **Chinese, Japanese and Korean text.** Game titles and host names in these scripts now draw
  with the console's own fonts instead of boxes.
- **Pick the host's screen.** The host card offers the screens a host shares, and a stream says
  at start when its screen is shared.
- **The device code while you wait for access.** It matches the code the host's web console
  shows beside your request.
- **Settings are never lost to a bad read.** A settings file that can't be read is kept as
  `settings.json.bad` instead of being replaced, so pairings survive.
- **Faster recovery after a lost packet** right after the picture has healed from another one.
- **A pad at rest sends no motion,** saving network calls while you don't move it.

Earlier versions: [1.1.0](https://github.com/punktfunk/client-switch/releases/tag/v1.1.0),
[1.0.0](https://github.com/punktfunk/client-switch/releases/tag/v1.0.0).

## Install

Copy `punktfunk.nro` to `/switch/punktfunk/` on the SD card, then open it by **holding R
while launching any installed game** (or from Sphaira). Applet mode, for example launching
from the Album, does not work.

See the [manual](https://github.com/punktfunk/client-switch/blob/main/docs/tester.md) for
pairing, controls and reporting a problem.

## What works

- **720p at 60 fps handheld**, tested extensively, with good performance and low latency.
- **H.264 and HEVC**, decoded by the console's hardware decoder, and **PyroWave**
  (experimental), decoded on the GPU.
- **Joy-Cons and the Pro Controller**, with HD rumble, gyro and the touch screen.
- **Stereo audio.**
- **Automatic bitrate**, capped at 20 Mb/s because the Switch's Wi-Fi is modest. You can set
  a fixed bitrate in the settings.
- **Two presentation modes.** *Smoothness* gives a very smooth stream at an acceptable
  latency. *Low latency* lowers the delay further, at the cost of some smoothness.

## Known limits

- Docked play has been tested less than handheld.
- At stock clocks it works, but raising the CPU clock makes the menu smoother and lowers the
  stream's latency.
- No launching from the Album (applet mode).
- No USB keyboard or mouse yet.

## Licence

The source is MIT OR Apache-2.0. `punktfunk.nro` is GPL-3.0-or-later because it links
FFmpeg's `nvtegra` NVDEC driver. Its corresponding source is this repository at this tag,
plus the FFmpeg recipe named in `THIRD-PARTY-NOTICES.txt`. `punktfunk.elf` is the unstripped
build, for resolving crash offsets.
