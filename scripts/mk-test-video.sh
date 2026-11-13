#!/usr/bin/env bash
# Two local test clips for chrome-smoke.py --only localvideo (no YouTube needed):
#   h264.mp4  H.264 + AAC,   vp9.webm  VP9 + Opus
# 150 s each, 640x360, a moving test pattern with a running clock and a 440 Hz tone.
# usage: scripts/mk-test-video.sh OUTDIR      (needs ffmpeg with libx264, libvpx-vp9, libopus)
set -euo pipefail
out=${1:?usage: mk-test-video.sh OUTDIR}
mkdir -p "$out"
src=(-f lavfi -i "testsrc2=size=640x360:rate=24" -f lavfi -i "sine=frequency=440:sample_rate=48000" -t 150)
nice -n 15 ffmpeg -hide_banner -loglevel error -y "${src[@]}" -c:v libx264 -preset veryfast -b:v 400k -pix_fmt yuv420p \
    -c:a aac -b:a 64k -ac 2 -movflags +faststart "$out/h264.mp4"
nice -n 15 ffmpeg -hide_banner -loglevel error -y "${src[@]}" -c:v libvpx-vp9 -b:v 400k -deadline realtime -cpu-used 8 \
    -c:a libopus -b:a 64k -ac 2 "$out/vp9.webm"
# the VP9/Opus clip in its two halves, to tell a video decoder fault from an audio one
nice -n 15 ffmpeg -hide_banner -loglevel error -y -i "$out/vp9.webm" -an -c:v copy "$out/vp9v.webm"
nice -n 15 ffmpeg -hide_banner -loglevel error -y -i "$out/vp9.webm" -vn -c:a copy "$out/opus.webm"
ls -l "$out"
