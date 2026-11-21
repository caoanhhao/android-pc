#!/bin/bash
# Compiles hyprtile and its unit tests with the AOSP tree's kotlinc and JDK and runs them on
# the host, without Soong (seconds instead of a Soong analysis). The same tests run under
# Soong as `hyprtile-tests` (java_test_host, Android.bp).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
AOSP=${AOSP:-$HERE/../../../aosp}
export JAVA_HOME=$AOSP/prebuilts/jdk/jdk21/linux-x86
KOTLINC=$AOSP/external/kotlinc/bin/kotlinc
JUNIT=$AOSP/prebuilts/devtools/tools/lib/junit-4.12.jar
HAMCREST=$AOSP/prebuilts/devtools/tools/lib/hamcrest-core-1.3.jar
STDLIB=$AOSP/external/kotlinc/lib/kotlin-stdlib.jar
OUT=$(mktemp -d)
trap '/usr/bin/rm -rf "$OUT"' EXIT
taskset -c 8-15 "$KOTLINC" -nowarn -jvm-target 17 -cp "$JUNIT" -d "$OUT" \
    $(find "$HERE/src" "$HERE/tests/src" -name '*.kt') 2>&1 | grep -v '^warning: unable to find kotlin-stdlib' || true
[ -n "$(find "$OUT" -name 'HyprTileTest.class')" ] || { echo "compile failed"; exit 1; }
classes=$(cd "$OUT" && find . -name '*Test.class' | sed 's|^\./||; s|\.class$||; s|/|.|g')
"$JAVA_HOME/bin/java" -cp "$OUT:$JUNIT:$HAMCREST:$STDLIB" org.junit.runner.JUnitCore $classes
