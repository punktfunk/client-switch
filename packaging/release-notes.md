punktfunk for the Nintendo Switch: stream games from your PC to a Switch 1 on Atmosphère.

**Install:** copy `punktfunk.nro` to `/switch/punktfunk/` on the SD card, then open it by
**holding R over any installed game**, not from the Album. The
[manual](https://github.com/punktfunk/client-switch/blob/main/docs/tester.md) covers pairing,
the controls and reporting a problem.

What works:

- 720p60 handheld and 1080p60 docked, H.264 or HEVC on the console's hardware decoder, and a
  dock change mid-stream without reconnecting.
- Joy-Cons and the Pro Controller with HD rumble and gyro, and the touch screen.
- Stereo audio, and an automatic bitrate up to 20 Mb/s.
- Sleep and wake reconnect to the stream.

Known limits: docked play is tested less than handheld, there is no launch from the Album,
and there is no USB keyboard or mouse yet.

**Licence.** The source is MIT OR Apache-2.0. `punktfunk.nro` is GPL-3.0-or-later because it
links FFmpeg's `nvtegra` NVDEC driver. Its corresponding source is this repository at this tag
plus the FFmpeg recipe named in `THIRD-PARTY-NOTICES.txt`. `punktfunk.elf` is the unstripped
build, for resolving crash offsets.
