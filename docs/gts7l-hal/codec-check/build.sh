#!/bin/bash
# Builds CodecCheck.java into codeccheck.dex with the AOSP tree's javac and d8, pushes it, and
# runs it with app_process:
#   docs/gts7l-hal/codec-check/build.sh [list | decode FILE [CODEC] | encode MIME [W H FRAMES]]
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$(git -C "$HERE" rev-parse --path-format=absolute --git-common-dir)/.." && pwd)"
A=$ROOT/aosp
OUT=${CODECCHECK_OUT:-$(mktemp -d)}
"$A/prebuilts/jdk/jdk21/linux-x86/bin/javac" --release 17 -d "$OUT" \
    -cp "$A/prebuilts/sdk/current/public/android.jar" "$HERE/CodecCheck.java"
"$A/out/host/linux-x86/bin/d8" --min-api 30 --output "$OUT" "$OUT"/CodecCheck*.class
mv "$OUT/classes.dex" "$OUT/codeccheck.dex"
ADB="adb -s ${SERIAL:?set SERIAL to the adb serial of the tablet}"
$ADB push "$OUT/codeccheck.dex" /data/local/tmp/ >/dev/null
[[ $# -eq 0 ]] && exit 0
$ADB shell CLASSPATH=/data/local/tmp/codeccheck.dex app_process /system/bin CodecCheck "$@"
