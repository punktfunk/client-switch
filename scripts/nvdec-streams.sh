#!/usr/bin/env bash
# Writes check 6's streams into dist/: scripts/nvdec-streams.sh. Ten seconds of 1080p60 H.264 and
# HEVC and two of 10-bit HEVC, shaped like the host's: no B-frames, one IDR, one slice, an access
# unit delimiter before every AU (the probe splits on them). Needs ffmpeg with libx264 and libx265.
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
out="$root/dist"
mkdir -p "$out"
src() { echo "testsrc2=size=1920x1080:rate=60:duration=$1"; }
ff=(ffmpeg -hide_banner -loglevel error -y -f lavfi)

"${ff[@]}" -i "$(src 10)" -c:v libx264 -profile:v high -pix_fmt yuv420p -bf 0 -g 600 -b:v 20M \
    -bsf:v h264_metadata=aud=insert -f h264 "$out/nvdec-h264.264"
"${ff[@]}" -i "$(src 10)" -c:v libx265 -pix_fmt yuv420p -b:v 20M \
    -x265-params bframes=0:keyint=600:log-level=error -bsf:v hevc_metadata=aud=insert -f hevc "$out/nvdec-hevc.265"
"${ff[@]}" -i "$(src 2)" -c:v libx265 -pix_fmt yuv420p10le -profile:v main10 -b:v 20M \
    -x265-params bframes=0:keyint=600:log-level=error -bsf:v hevc_metadata=aud=insert -f hevc "$out/nvdec-hevc10.265"
ls -l "$out"/nvdec-*
