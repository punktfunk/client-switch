# Improvements

Changes the console's runs point to, for later. Each needs the maintainer's yes before it lands:
the first changes what a player sees by default, the second lives in punktfunk's core.

## The smoothness buffer's Automatic on the Switch

The shared setting resolves Automatic to two frames on every client
(`pf-client-core::trust::PresentPriority::resolve`). This client adds the frames on top of
core's adaptive cushion (`session/timeline.rs`), so one frame here already holds up to about
two refreshes. On the console, one frame presented 59–60 pictures a second at 15–23 ms decode to
present; two frames presented the same at 31–40 ms (NOTES, 2026-10-31). The proposal is that
Automatic picks one frame on the Switch, through a per-platform default in the shared crate or
an override in this client.

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
