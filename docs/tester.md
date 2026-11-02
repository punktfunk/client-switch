# punktfunk on the Nintendo Switch

punktfunk streams games from your PC to a Switch over your home network. This page takes you
from download to playing, and says what to send back when something goes wrong.

## What you need

- **A Switch 1** (any model, including the Lite and the OLED) running homebrew through
  Atmosphère, with the Homebrew Menu. The Switch 2 is not supported.
- **A punktfunk host** on the same network: install it on the PC that runs your games
  ([install guide](https://docs.punktfunk.unom.io/docs/install)).
- **A good network.** Wi-Fi at 5 GHz near the router, or a wired dock. The PC should be wired.

## Install

1. Download `punktfunk.nro` from the latest release.
2. Copy it to `/switch/punktfunk/punktfunk.nro` on the SD card.

To update, replace the file. Your hosts, pairing and settings are kept.

## Launch it the right way

**Hold R while you open any installed game**, then pick punktfunk in the Homebrew Menu. That
runs it with the console's full memory.

**Do not open it from the Album.** The Album runs homebrew with a small slice of memory, and
punktfunk crashes there before the first menu.

## First connection

1. Start the host on your PC.
2. Open punktfunk. Hosts on your network appear on the **Hosts** screen within a few seconds.
3. Pick your host. The title asks for a PIN.
4. In the host's web console, open **Devices**, click **Pair a device** and then **Show PIN**.
   Enter that PIN on the Switch. The PIN lasts two minutes.
5. The host's games show under **Games**. Pick one to start the stream.

You pair once per host. After that, picking the host connects straight away.

## Controls

In the menus, **A confirms and B goes back**, as everywhere on the Switch. **L and R** switch
tabs.

In a stream, the pads, the touch screen and gyro go to the PC. These do not:

| Press | Does |
| --- | --- |
| **Minus + A** | Opens the quick actions dial: statistics, controller mouse, controller type. Press it again or B to close it. |
| **L + R + Plus + Minus**, held for one second | Ends the stream and returns to the menu. |

Joy-Cons attached to the console show up on the PC as one Joy-Con pair, and a Pro Controller
as a Switch Pro controller. HD rumble works on both.

## Handheld and docked

Handheld streams at 1280×720, docked at 1920×1080, both at 60 frames per second. Docking or
undocking during a stream changes the size without reconnecting.

If you put the console to sleep during a stream, it reconnects to the same stream when it wakes.

## Settings worth knowing

- **Bitrate: Automatic** is the default and the best choice. It goes up to 20 Mb/s and lowers
  itself when the network gets worse.
- **Settings > About** shows the version. Give it when you report a problem.

## Known limits

- **Docked play has been tested less than handheld.** Docked at 1080p, a busy Wi-Fi can make
  the picture fall behind. A wired dock helps, and so does a fixed bitrate of 10–12 Mb/s.
- Automatic stops at 20 Mb/s. The console's processor, at its normal clock, is the limit.
- No launch from the Album (see above).
- No USB keyboard or mouse yet.

## Reporting a problem

Each launch writes a log to the SD card:

- `/switch/punktfunk/logs/punktfunk-switch-<version>.log`, the current launch;
- the same name ending in `.log.1`, `.log.2` and so on, the launches before it.

If the title crashed, send the log of the crashed launch, which is the `.log.1` after you open
the title again. Say what you were doing, whether the console was handheld or docked, and on
Wi-Fi or wired. A photo of the screen helps.
