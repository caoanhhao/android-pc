#!/bin/bash
# Makes the test clips of security-usb-media.md §5 in DIR (default: ./clips) with the host's ffmpeg:
# 1280x720 30 fps 3 s video in every codec, 48 kHz audio, the v4l2dec streams (.hdr) and a 20 s
# 1080p H.264 + AAC clip for playing in an app (gts7l-test-1080p.mp4).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "${1:-clips}" && cd "${1:-clips}"
F="taskset -c 8-15 nice ffmpeg -hide_banner -loglevel error -y"
V="-f lavfi -i testsrc2=size=1280x720:rate=30:duration=3"
A="-f lavfi -i sine=frequency=440:duration=3:sample_rate=48000"
$F $V $A -c:v libx264 -pix_fmt yuv420p -c:a aac -shortest avc_aac.mp4
$F $V -c:v libx265 -pix_fmt yuv420p -tag:v hvc1 -x265-params log-level=error hevc.mp4
$F $V $A -c:v libvpx -b:v 2M -c:a libopus -shortest vp8_opus.webm
$F $V -c:v libvpx-vp9 -b:v 2M vp9.webm
$F $V -c:v libsvtav1 -preset 10 av1.mp4
$F $A -c:a libmp3lame mp3.mp3
$F $A -c:a flac flac.flac
$F $A -c:a libopus opus.ogg
$F $V -c:v libx264 -profile:v high -pix_fmt yuv420p -x264-params aud=1 -bf 0 -f h264 avc.h264
$F $V -c:v libx265 -pix_fmt yuv420p -x265-params aud=1:log-level=error -f hevc hevc.h265
$F -i vp9.webm -c copy -f ivf vp9.ivf
python3 -I "$HERE/v4l2-hdr.py" h264 avc.h264 avc.hdr
python3 -I "$HERE/v4l2-hdr.py" hevc hevc.h265 hevc.hdr
python3 -I "$HERE/v4l2-hdr.py" ivf vp9.ivf vp9.hdr
$F -f lavfi -i testsrc2=size=1920x1080:rate=30:duration=20 \
    -f lavfi -i sine=frequency=660:duration=20:sample_rate=48000:beep_factor=4 \
    -c:v libx264 -pix_fmt yuv420p -c:a aac -shortest gts7l-test-1080p.mp4
