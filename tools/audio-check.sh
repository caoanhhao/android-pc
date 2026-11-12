#!/bin/bash
# End-to-end audio check through the framework: builds tools/audio-check/AudioCheck.java
# into a dex (AOSP's prebuilt JDK, android.jar and d8) and runs it on the device with
# app_process.
#
#   ADB="adb -s 0.0.0.0:6520" tools/audio-check.sh play 3 440
#   ADB="adb -s 127.0.0.1:5556" tools/audio-check.sh record 3
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
A=$ROOT/aosp
OUT=$ROOT/out/audio-check
ADB=${ADB:-adb}
if [[ ! -f $OUT/classes.dex || $ROOT/tools/audio-check/AudioCheck.java -nt $OUT/classes.dex ]]; then
    rm -rf "$OUT" && mkdir -p "$OUT/classes"
    "$A/prebuilts/jdk/jdk21/linux-x86/bin/javac" --release 11 -cp "$A/prebuilts/sdk/current/public/android.jar" \
        -d "$OUT/classes" "$ROOT/tools/audio-check/AudioCheck.java"
    "$A/prebuilts/jdk/jdk21/linux-x86/bin/java" -cp "$A/prebuilts/r8/r8.jar" com.android.tools.r8.D8 --min-api 35 --lib "$A/prebuilts/sdk/current/public/android.jar" \
        --output "$OUT" "$OUT"/classes/*.class
fi
$ADB push "$OUT/classes.dex" /data/local/tmp/audio-check.dex >/dev/null
$ADB shell CLASSPATH=/data/local/tmp/audio-check.dex app_process /system/bin AudioCheck "$@"
