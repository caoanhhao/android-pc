#!/bin/bash
# Builds touchinject.dex (multi-finger touch injection for the D6 tests) with the AOSP tree's
# JDK and d8, without Soong. Output: $1 or ./touchinject.dex. Push it to /data/local/tmp and run
#   CLASSPATH=/data/local/tmp/touchinject.dex app_process / TouchInject pinch 1270 600 300 80 300
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
AOSP=${AOSP:-$HERE/../../../../aosp}
OUTFILE=$(realpath -m "${1:-touchinject.dex}")
export JAVA_HOME=$AOSP/prebuilts/jdk/jdk21/linux-x86
TMP=$(mktemp -d)
trap '/usr/bin/rm -rf "$TMP"' EXIT
"$JAVA_HOME/bin/javac" -source 11 -target 11 -nowarn \
    -cp "$AOSP/prebuilts/sdk/current/public/android.jar" -d "$TMP/classes" "$HERE/TouchInject.java"
"$JAVA_HOME/bin/java" -cp "$AOSP/prebuilts/r8/r8.jar" com.android.tools.r8.D8 --min-api 30 --output "$TMP" $(find "$TMP/classes" -name '*.class')
cp "$TMP/classes.dex" "$OUTFILE"
echo "$OUTFILE"
