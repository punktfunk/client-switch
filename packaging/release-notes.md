First release of punktfunk for the Nintendo Switch: stream games from your PC to a Switch 1
running Atmosphère.

## Install

Copy `punktfunk.nro` to `/switch/punktfunk/` on the SD card, then open it by **holding R
while launching any installed game** (or from Sphaira). Applet mode, for example launching
from the Album, does not work.

See the [manual](https://github.com/punktfunk/client-switch/blob/main/docs/tester.md) for
pairing, controls and reporting a problem.

## What works

- **720p at 60 fps handheld**, tested extensively, with good performance and low latency.
- **H.264 and HEVC**, decoded by the console's hardware decoder.
- **Joy-Cons and the Pro Controller**, with HD rumble, gyro and the touch screen.
- **Stereo audio.**
- **Automatic bitrate**, capped at 20 Mb/s because the Switch's Wi-Fi is modest. You can set
  a fixed bitrate in the settings.
- **Two presentation modes.** *Smoothness* gives a very smooth stream at an acceptable
  latency. *Low latency* lowers the delay further, at the cost of some smoothness.

## Known limits

- Docked play has been tested less than handheld.
- No launching from the Album (applet mode).
- No USB keyboard or mouse yet.

## Licence

The source is MIT OR Apache-2.0. `punktfunk.nro` is GPL-3.0-or-later because it links
FFmpeg's `nvtegra` NVDEC driver. Its corresponding source is this repository at this tag,
plus the FFmpeg recipe named in `THIRD-PARTY-NOTICES.txt`. `punktfunk.elf` is the unstripped
build, for resolving crash offsets.
